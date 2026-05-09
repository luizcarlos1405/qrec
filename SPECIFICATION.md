# qrec — Quick Recording TUI

A terminal user interface for quick screen recording on Wayland, powered by `wf-recorder`.

## Overview

`qrec` is a Rust TUI application that wraps `wf-recorder` to provide an interactive screen recording workflow. It manages a timeline of recording chunks, allowing the user to record, preview, reorder, discard, and concatenate video segments — all from the terminal using vim-like keybindings. It also supports automatic silence trimming (autotrim) and audio delay compensation.

## Architecture

### Layer Separation

The codebase is split into three layers with strict boundaries:

1. **Pure functions** — all business logic (timeline management, chunk ordering, JSON serialization, filename generation, command construction, silence trim point computation). These functions take data in and return data out with zero side effects. They are fully unit-testable.
2. **Side-effect boundary** — a thin adapter module that interacts with the outside world: spawning `wf-recorder` / `ffmpeg` / `ffprobe` / `mpv` processes, reading `wf-recorder -L` output, reading/writing `qrec.json`, and listing directory contents.
3. **TUI layer** — renders the UI and maps user input to calls into the pure and side-effect layers.

### Project Structure

```
src/
├── main.rs              # entry point, event loop, translate_key, execute_command, trim spawning
├── app.rs               # application state machine (pure): Key, AppState, AppCommand, AppEvent
├── timeline.rs          # timeline / chunk logic (pure): add, remove, reorder, zoom, cut mask
├── config.rs            # qrec.json read/write types and defaults (pure serde)
├── command.rs           # builds shell command structs from intent (pure)
├── effects.rs           # side-effect adapter (process spawn, fs, wf-recorder queries, silence detect)
└── ui.rs                # ratatui rendering (side-effect: terminal drawing only)
```

### Dev Environment

A `shell.nix` (or `flake.nix`) provides:

- `rustc`, `cargo`, `rustfmt`, `clippy`
- `wf-recorder`
- `ffmpeg` / `ffprobe`
- `mpv`
- `pulseaudio` / `pipewire` (for `pactl`)

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
      "recorded_at": "2026-05-03T14:22:01Z",
      "trim_start": 3.2,
      "trim_end": 12.4
    }
  ],
  "selected_screen": "HDMI-A-1",
  "selected_microphone": "alsa_output.pci-0000_00_1f.3.analog-stereo",
  "next_chunk_number": 2,
  "autotrim_enabled": false,
  "autotrim_threshold_db": -40.0,
  "audio_delay_secs": 0.0,
  "autotrim_padding_secs": 0.0
}
```

- `chunks` — ordered list of all chunks in the timeline. The order in this array is the final render order.
- `next_chunk_number` — monotonically increasing counter. Never resets, even after deletions.
- `selected_screen` / `selected_microphone` — last-used device, restored on startup.
- `autotrim_enabled` — whether automatic silence trimming is active.
- `autotrim_threshold_db` — silence detection threshold in dB (range: -60 to -5, default: -40).
- `audio_delay_secs` — audio delay compensation in seconds (range: -5 to +5, default: 0).
- `autotrim_padding_secs` — padding added around autotrim cut points in seconds (range: 0 to 5, default: 0).

### Chunk

Each chunk has:
- `id` — identifier like `chunk-1`.
- `file` — filename like `chunk-1.mp4`.
- `duration_secs` — video duration in seconds.
- `recorded_at` — ISO 8601 timestamp.
- `trim_start` / `trim_end` — optional trim points (set by autotrim), `null` when not trimmed.

### Chunk Naming

Files are saved in the current working directory as `chunk-{n}.mp4` where `n` is `next_chunk_number` at the time of recording. `next_chunk_number` increments after each recording starts. Deleted chunks have their files removed from disk and their entries removed from `qrec.json`.

## TUI Layout

```
┌──────────────────────────────────────────────────────┐
│  qrec — Quick Recording                              │
│  j/k Navigate  │  h/l Change  │  r Record  │ ...     │
├──────────────────────────────────────────────────────┤
│ ▸ Screen:             HDMI-A-1                   ◄ ► │
│   Microphone:         No microphone                  │
│   Audio delay:        +0.00s                         │
│   Autotrim:           off                            │
│   Autotrim threshold: -40dB                          │
│   Autotrim padding:   0.0s                           │
│   ██▓▓██▓▓▓▓████████▓▓▓▓▓▓                          │
├──────────────────────────────────────────────────────┤
│                                                      │
│  (log entries from logs.txt)                         │
│                                                      │
├──────────────────────────────────────────────────────┤
│  Ready                                               │
└──────────────────────────────────────────────────────┘
```

### Controls Panel

The controls panel is a bordered area with 7 rows navigated with `j`/`k`:

1. **Screen** — cycle available outputs with `h`/`l`.
2. **Microphone** — cycle audio sources with `h`/`l`. First option is always "No microphone".
3. **Audio delay** — adjust with `h`/`l` in ±0.01s steps, range -5.0 to +5.0.
4. **Autotrim** — toggle on/off with `h`/`l`.
5. **Autotrim threshold** — adjust with `h`/`l` in ±1dB steps, range -60 to -5.
6. **Autotrim padding** — adjust with `h`/`l` in ±0.1s steps, range 0.0 to 5.0.
7. **Timeline** — the horizontal strip of chunks.

The currently active row shows a `▸` prefix and `◄ ►` suffix. Autotrim threshold and padding rows are visually dimmed when autotrim is disabled.

### Timeline Strip

The timeline is rendered inline as the last row of the controls panel. Chunks are rendered as contiguous blocks (`█`) with `│` separators between them.

- **Width calculation** — each chunk's character width is `ceil(chunk_frames / frames_per_char)`, where `chunk_frames = duration_secs * fps` (fps defaulting to 30).
- **Selected chunk** — rendered in green bold (`█`). Unselected chunks are dark gray.
- **Autotrim cut regions** — when autotrim is enabled, trimmed-away regions within each chunk are rendered as `░` (dim) instead of `█`.
- **Empty state** — when no chunks exist and not recording, the timeline row is empty.
- **Recording indicator** — while recording, a pulsing block (`▒`) in red bold grows at the end of the strip.
- **Horizontal scrolling** — when the total rendered width exceeds the viewport, the strip scrolls. The viewport auto-pans to keep the selected chunk visible.

### Timeline Zoom and Panning

The zoom level controls `frames_per_char` — how many video frames each terminal character represents.

- **Maximum zoom in** (`i`) — `frames_per_char = 1`.
- **Zoom out** (`o`) — doubles `frames_per_char` (1 → 2 → 4 → 8 → ...). Prevented if the result would collapse the entire timeline to a single character.
- **Zoom in** (`i`) — halves `frames_per_char`, down to a minimum of 1.
- **Default zoom** — chosen so that the total timeline fits within the viewport width: `frames_per_char = ceil(total_frames / viewport_width)`, rounded up to the nearest power of 2.

The viewport auto-pans to keep the selected chunk visible after navigation or zoom changes.

### Logs Panel

The area below the controls panel shows the last 100 lines from `logs.txt`, refreshed every 2 seconds. This includes wf-recorder spawn/exit logs, ffmpeg errors, and autotrim detection results.

### Status Bar

The bottom row displays:

- **Ready** (dark gray background) — status messages.
- **Recording** (red background) — shows `● REC {elapsed}s — Recording...`.
- **Exporting** (yellow background) — shows `Exporting...`.
- Error messages, action confirmations, chunk info when navigating the timeline.

Messages persist until the next action overwrites them.

## Keybindings

| Key       | Context   | Action                                                        |
| --------- | --------- | ------------------------------------------------------------- |
| `q`       | global    | Quit (stops recording first if in progress)                   |
| `j` / `k` | Ready     | Move down/up between control rows (wraps around)              |
| `h` / `l` | Ready     | Context-dependent on active row (see Controls Panel above)    |
| `r`       | global    | Start recording (when Ready) / Stop recording (when Recording)|
| `e`       | Ready     | Export (concatenate) all chunks into `output.mp4`             |
| `d`       | Ready     | Delete the currently selected chunk (file + entry)            |
| `p`       | Ready     | Preview the selected chunk in mpv                             |
| `P`       | Ready     | Preview all chunks in order in mpv                            |
| `H` / `L` | Timeline  | Reorder selected chunk left/right in the timeline             |
| `i` / `o` | Timeline  | Zoom in / zoom out the timeline scale                         |
| `y` / `n` | Overwrite | Confirm/deny overwriting `output.mp4`                         |

### Keybinding Notes

- There is no Tab key — all navigation uses `j`/`k` through a single vertical list of control rows.
- `r` is dual-purpose: starts recording when Ready, stops when Recording. It is blocked during Exporting.
- `e` triggers export. If `output.mp4` already exists, the user is prompted with `y`/`n` to overwrite.
- `d` always deletes the selected chunk regardless of which control row is active. If no chunks exist, the status bar shows "No chunk to delete".
- `H` and `L` only work when the Timeline row is active. They swap the selected chunk with its neighbor and persist to `qrec.json`.
- `p` previews the selected chunk. If autotrim is enabled, it previews only the trimmed region. If the file doesn't exist, it shows an error.
- `P` previews all chunks in order. If autotrim is enabled, it uses an mpv EDL file to play only the kept regions.
- `p` and `P` are blocked while recording.
- During recording, only `r` (stop) and `q` (quit) are accepted. All other keys are ignored.

## Functional Requirements

### FR-1: Screen Discovery

On startup, `qrec` runs `wf-recorder -L` and parses the output to build the list of available outputs. These populate the screen selector. If only one output exists, it is auto-selected. The previously selected screen (from `qrec.json`) is restored if still available.

### FR-2: Microphone Discovery

On startup, `qrec` runs `pactl list short sources` to list available audio sources. These populate the microphone selector. "No microphone" is always the first entry (index 0). The previously selected microphone is restored if still available.

### FR-3: Recording

When the user starts a recording (`r`):

1. Determine `next_chunk_number` from `qrec.json` and generate the filename `chunk-{n}.mp4`.
2. Increment `next_chunk_number` and save config.
3. Construct the `wf-recorder` command:
   ```
   wf-recorder --output <selected_screen> --audio <selected_mic> --file chunk-<n>.mp4
   ```
   If "No microphone" is selected, omit the `--audio` flag.
4. Spawn the process in the background. Record the start time with `std::time::Instant`.
5. The TUI enters recording mode: all input is locked except `r` (stop) and `q` (quit). The status bar shows "Recording..." with elapsed time.
6. On stop, the process receives `SIGINT`. Once exited, the chunk's duration is read via `ffprobe`. If the file doesn't exist, an error is shown.
7. The new chunk is appended to `chunks` in `qrec.json`. Status bar shows "Recorded chunk-<n>.mp4". If autotrim is enabled, a background trim computation is triggered.

### FR-4: Chunk Deletion

`d` deletes the currently selected chunk. The entry is removed from the `chunks` array and the `.mp4` file is deleted from disk. The trim cache entry is removed. Chunk numbers are not reused. `next_chunk_number` is never decremented.

### FR-5: Chunk Reordering

`H` moves the selected chunk one position earlier in the array. `L` moves it one position later. Only works when the Timeline row is active. The change is persisted to `qrec.json` immediately.

### FR-6: Export (Concatenate)

Pressing `e` exports all chunks into `output.mp4`:

- If `output.mp4` exists, the user is prompted to confirm overwrite (`y`/`n`).
- With autotrim **disabled**: uses `ffmpeg -f concat -safe 0 -i <file_list> -c copy output.mp4`.
- With autotrim **enabled** and cached trim points: trims each chunk individually with ffmpeg, then concatenates the trimmed files.
- With autotrim **enabled** but no cache: runs silence detection on each chunk before trimming and concatenating.
- If audio delay is non-zero, applies `adelay` (positive) or `atrim` (negative) during concatenation.
- Temporary files (`qrec-trim-*.mp4`) are created and cleaned up during autotrim export.
- The TUI shows "Exporting..." during the operation.

### FR-7: Preview Chunk

Pressing `p` previews the selected chunk:

- With autotrim **disabled**: `mpv chunk-<n>.mp4`.
- With autotrim **enabled**: `mpv --start=<trim_start> --end=<trim_end> chunk-<n>.mp4`.
- If audio delay is non-zero, `--audio-delay=` is passed to mpv.
- mpv runs in the foreground; the TUI is suspended and resumes when mpv exits.

### FR-8: Preview Timeline

Pressing `P` previews all chunks in order:

- With autotrim **disabled**: `mpv chunk-1.mp4 chunk-2.mp4 ...`.
- With autotrim **enabled**: generates an mpv EDL file with trimmed segments.
- If audio delay is non-zero, `--audio-delay=` is passed to mpv.
- The TUI is suspended and resumes when mpv exits.

### FR-9: State Persistence

`qrec.json` is written after every mutation:

- Chunk added, deleted, or reordered
- Screen or microphone selection changed
- Recording started (`next_chunk_number` incremented)
- Audio delay, autotrim settings, threshold, or padding changed
- Autotrim cache computation completed

On startup, `qrec.json` is read to restore state. If it does not exist, it is created with defaults.

### FR-10: Autotrim (Silence Detection)

When autotrim is enabled:

1. A background thread runs `ffmpeg silencedetect` on each chunk to find silence intervals.
2. Leading silence (starting within 0.5s of chunk start) and trailing silence (ending within 0.5s of chunk end) are detected.
3. Padding is applied: trim points are expanded by `autotrim_padding_secs`, clamped to [0, duration].
4. Results are cached in memory (`trim_cache`) and persisted to `qrec.json` chunk entries.
5. The cache is invalidated (epoch incremented) when: autotrim is toggled on, threshold changes, padding changes, or a new recording completes.
6. Cache computation is debounced (500ms) to avoid redundant work during rapid setting changes.
7. The timeline visually shows cut regions with `░` characters.
8. Preview and export use the cached trim points.

### FR-11: Audio Delay Compensation

- Audio delay is adjustable from -5.0s to +5.0s in 0.01s steps.
- **Positive delay**: shifts audio later using ffmpeg `adelay` filter during export; `--audio-delay=` flag during mpv preview.
- **Negative delay**: trims audio from the start using ffmpeg `atrim` filter during export; `--audio-delay=` flag during mpv preview.
- When delay is 0.0 (default), no audio processing is applied.

### FR-12: Quit

Pressing `q` quits the TUI. If a recording is in progress, it is stopped first (SIGINT sent to wf-recorder). No confirmation prompt is shown — the recording is stopped and the app exits.

### FR-13: Dependency Check

On startup, `qrec` checks that all required runtime tools are available: `wf-recorder`, `ffmpeg`, `ffprobe`, `mpv`, `pactl`. If any are missing, an error is printed and the application exits.

### FR-14: Log File

All significant operations (recording start/stop, ffmpeg commands, silence detection results, errors) are appended to `logs.txt` in the working directory with timestamps. The TUI displays the last 100 lines of this log in the logs panel, refreshed every 2 seconds.

## Dependency Graph

```
qrec
├── ratatui          (TUI framework)
├── crossterm        (terminal backend)
├── serde + serde_json  (qrec.json serialization)
├── anyhow           (error handling)
├── tempfile         (temporary concat list for ffmpeg, EDL files for mpv)
├── chrono           (timestamps, serde integration)
└── libc             (SIGINT to wf-recorder)
```

Runtime dependencies (provided by nix-shell):

- `wf-recorder`
- `ffmpeg` / `ffprobe`
- `mpv`
- `pulseaudio` / `pipewire` (for `pactl`)

## File Outputs

| File               | Purpose                                          |
| ------------------ | ------------------------------------------------ |
| `chunk-{n}.mp4`    | Individual recorded segments                     |
| `qrec.json`        | Project state (timeline, devices, settings)      |
| `output.mp4`       | Final rendered concatenation                     |
| `logs.txt`         | Error log (appended, auto-created)               |
| `qrec-trim-*.mp4`  | Temporary trimmed files (created/deleted during autotrim export) |

## Testing Strategy

Because the pure layer contains all business logic, tests focus there:

- **timeline tests** — add, remove, reorder chunks; verify array order and next_chunk_number; zoom calculations; chunk width computations; cut mask generation for autotrim visualization.
- **command tests** — given a recording intent, verify the correct `wf-recorder` command string; verify ffmpeg concat commands with and without audio delay; verify silence detect command; verify trim command; verify mpv preview commands with start/end and audio delay.
- **config tests** — serialize/deserialize `qrec.json`; verify round-trip fidelity including autotrim and audio delay fields.
- **app state tests** — simulate key events and verify state transitions (row navigation, selection changes, recording state machine, overwrite prompts, autotrim toggle, threshold/padding adjustment, audio delay adjustment, trim cache epoch management).
- **effects tests** — silence detection trim point computation (pure function: `compute_trim_points`).

The side-effect layer (process spawning, filesystem) is tested with integration tests in the nix-shell environment.

## Error Handling

- If `wf-recorder`, `ffmpeg`, `ffprobe`, `mpv`, or `pactl` are not found, display an error and exit before starting the TUI.
- If `wf-recorder` fails to spawn, roll back `next_chunk_number` and show an error.
- If a recorded file is missing after wf-recorder stops, show an error referencing `logs.txt`.
- If `ffmpeg` concatenation fails, surface the error output to the status bar.
- If `mpv` fails to launch, show an error in the status bar.
- All errors are logged to `logs.txt` with timestamps.
