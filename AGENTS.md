# AGENTS.md

## Commands

```sh
nix-shell            # enter dev shell (provides rustc, cargo, clippy, rustfmt, wf-recorder, ffmpeg, mpv, pulseaudio)
cargo build          # debug build
cargo build --release
cargo install --path . --root ~/.local   # installs binary to ~/.local/bin/qrec
cargo clippy         # lint
cargo fmt --check    # format check
cargo test           # run ~116 unit tests (pure core only)
cargo run            # run the TUI (requires wf-recorder, ffmpeg, ffprobe, mpv, pactl at runtime)
```

## Architecture

Functional Core / Imperative Shell (FCIS) — strictly enforced.

### Dependency graph

```
config.rs ← timeline.rs ← app.rs → command.rs
    ↑                          ↑
    └────── (core boundary) ───┘
               ↑
     ┌─────────┼─────────┐
     │         │         │
  main.rs   effects.rs  ui.rs
  (shell)   (shell)    (view)
```

**Arrows point from importer to importee. The core never imports from the shell. Ever.**

### Core layer — zero side effects, zero I/O types

Files: `config.rs`, `timeline.rs`, `command.rs`, `app.rs`

- No `crossterm`, `std::process`, `std::fs`, `std::time::Instant`, or any I/O type.
- `app.rs` defines domain types: `Key` (not crossterm's), `ScreenInfo`, `ControlsRow`, `AppState`, `AppCommand`, `AppEvent`.
- `handle_key(app: &App, key: Key) -> (App, Vec<AppCommand>)` — pure function. Takes immutable app, returns new app + commands for the shell.
- `apply_event(app: &App, event: AppEvent) -> (App, Vec<AppCommand>)` — pure function. Shell feeds results back; core decides new state and may emit follow-up commands (e.g. SaveConfig after trim cache completes).
- `timeline.rs` functions take owned `QrecConfig` and return it (no `&mut`).
- `command.rs` builds `Command` structs (data only, no process spawning). Note: `concat_list_content` uses `std::fs::canonicalize` for resolving absolute paths — this is the one minor I/O exception in the core, tolerated because it's purely for constructing command data.
- `config.rs` has pure serialization/deserialization on string input/output.

### Shell layer — all side effects

Files: `main.rs`, `effects.rs`

- `main.rs` owns the event loop: poll crossterm → `translate_key()` → `handle_key()` → `execute_command()` → `apply_event()` → loop.
- `translate_key()` maps `crossterm::event::KeyEvent` → domain `Key` at the boundary.
- `execute_command()` maps `AppCommand` → side effects → returns `Vec<AppEvent>`.
- Shell owns `std::time::Instant` for recording timing; passes `f64` elapsed to core.
- `effects.rs` handles all process spawning, file I/O, filesystem checks, silence detection, and trim computation.
- `main.rs` spawns background threads for autotrim computation, communicating via `mpsc::channel` with `TrimMsg` messages.

### Two-phase protocol

1. **Core emits `AppCommand`** — what it wants the shell to do (start recording, save config, export, preview, delete, check overwrite, refresh trim cache)
2. **Shell executes side effect** — spawns processes, writes files, checks filesystem
3. **Shell feeds `AppEvent` back to core** — result of the side effect (success, failure, data)
4. **Core decides new state via `apply_event`** — all state transitions happen here, never in the shell. Core may emit additional commands (e.g. `SaveConfig` after `TrimCacheComplete`).

### TUI layer — rendering only

Files: `ui.rs`

- Ratatui rendering. Reads `App` state, draws to terminal. No mutation, no I/O.
- Layout: header (title + context-sensitive shortcuts), controls panel (7 rows), logs panel, status bar.

## Key types

### AppState
- `Ready` — normal interactive mode
- `Recording` — wf-recorder running, most keys blocked
- `Exporting` — ffmpeg running, all keys blocked
- `Exited` — app should terminate

### ControlsRow (navigated with j/k)
- `Screen` → `Microphone` → `AudioDelay` → `Autotrim` → `AutotrimThreshold` → `AutotrimPadding` → `Timeline` → wraps to `Screen`

### AppCommand variants
- `SaveConfig`
- `StartRecording { screen, mic, filename }`
- `StopRecording`
- `DeleteChunk(usize)`
- `CheckOverwriteThenExport { files, output }` — checks if output.mp4 exists first
- `Export { files, output }` — direct export (after overwrite confirmed or no conflict)
- `PreviewChunk(usize)`
- `PreviewAll`
- `RefreshTrimCache` — signals main loop to re-run autotrim computation

### AppEvent variants
- `RecordingStarted`, `RecordingFailed`, `RecordingStopped`, `RecordingFileMissing`
- `ConfigSaved`, `ConfigSaveFailed`
- `FileDeleted`, `FileDeleteFailed`
- `RenderSucceeded`, `RenderFailed`
- `OverwriteCheckResult { exists, files, output }`
- `PreviewDone`
- `TrimCacheEntry { epoch, chunk_id, trim_start, trim_end }` — from background thread
- `TrimCacheComplete { epoch }` — from background thread
- `LogContentUpdated { lines }`

## Runtime files (in CWD)

| File | Purpose |
|---|---|
| `qrec.json` | Project state (chunks, devices, settings). Auto-created on first run. |
| `chunk-{n}.mp4` | Recorded segments |
| `output.mp4` | Final render |
| `logs.txt` | Error log (appended, auto-created) |
| `qrec-trim-*.mp4` | Temporary trimmed files during autotrim export (created and deleted) |

## Key constraints

- `edition = "2021"` in Cargo.toml
- `libc` is a direct dependency (used for `SIGINT` to `wf-recorder`)
- `chrono` is a direct dependency (used for timestamps and serde)
- Recording uses `wf-recorder` — Wayland only, not X11
- `pactl` (pulseaudio CLI) is used for microphone discovery; the first entry in the mic list is always "No microphone" (mute)
- `ffprobe` is used for video duration detection (runtime dependency)
- Chunk numbers never reset, even after deletion
- Config is written after every mutation (add/delete/reorder/device change/recording start/setting change/trim cache update)
- Autotrim computation runs in a background thread with 500ms debounce, communicating via `mpsc::channel`
- Trim cache is epoch-based: stale results from outdated computations are discarded

## Rules for modifying code

1. **Never import I/O types into core modules.** If you need `crossterm`, `std::fs`, `std::process`, or `std::time::Instant` in `app.rs`, `timeline.rs`, `command.rs`, or `config.rs`, you are violating the architecture. Put it in `main.rs` or `effects.rs` instead. (Exception: `command.rs::concat_list_content` uses `std::fs::canonicalize`.)
2. **Never mutate `App` in the shell.** State transitions go through `apply_event()`. The shell does not set `app.state`, `app.status_message`, etc. directly — it emits events and the core handles them.
3. **New user actions go through the two-phase protocol.** Add a variant to `AppCommand` (core intent) and `AppEvent` (shell result). Wire `execute_command()` in `main.rs` to bridge them.
4. **`handle_key` must stay pure.** It takes `&App` and returns `(App, Vec<AppCommand>)`. No side effects inside it.
5. **Timeline functions take and return owned `QrecConfig`.** No `&mut QrecConfig` parameters. Use `std::mem::take` + reassign at call sites.
6. **Autotrim features use the epoch-based cache.** New features that modify trim behavior must increment `trim_cache_epoch` and handle stale `TrimMsg` results correctly.
