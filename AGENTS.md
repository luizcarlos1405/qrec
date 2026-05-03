# AGENTS.md

## Commands

```sh
nix-shell            # enter dev shell (provides rustc, cargo, clippy, rustfmt, wf-recorder, ffmpeg, mpv, pulseaudio)
cargo build          # debug build
cargo build --release
cargo clippy         # lint
cargo fmt --check    # format check
cargo run            # run the TUI (requires wf-recorder, ffmpeg, mpv, pactl at runtime)
```

No test suite exists yet. Pure-layer unit tests are planned for `timeline.rs`, `command.rs`, `config.rs`, and `app.rs`.

## Architecture

Three-layer design, strictly separated:

- **Pure layer** (`config.rs`, `timeline.rs`, `command.rs`, `app.rs`) — zero side effects. All business logic. `app.rs` is the state machine; key events return `Vec<AppAction>` that the main loop dispatches.
- **Side-effect boundary** (`effects.rs`) — all process spawning (`wf-recorder`, `ffmpeg`, `mpv`, `ffprobe`, `pactl`), file I/O, and config persistence. Errors are logged to `logs.txt` via `log_error()`.
- **TUI layer** (`ui.rs`) — ratatui rendering only. Reads `App` state, draws to terminal.

`main.rs` owns the event loop: polls crossterm events → `app.handle_key()` → dispatches `AppAction`s → loops.

## Runtime files (in CWD)

| File | Purpose |
|---|---|
| `qrec.json` | Project state (chunks, devices, counter). Auto-created on first run. |
| `chunk-{n}.mp4` | Recorded segments |
| `output.mp4` | Final render |
| `logs.txt` | Error log (appended, auto-created) |

## Key constraints

- `edition = "2021"` in Cargo.toml
- `libc` is a direct dependency (used for `SIGINT` to `wf-recorder`)
- Recording uses `wf-recorder` — Wayland only, not X11
- `pactl` (pulseaudio CLI) is used for microphone discovery; the first entry in the mic list is always "No microphone" (mute)
- Chunk numbers never reset, even after deletion
- Config is written after every mutation (add/delete/reorder/device change/recording start)
