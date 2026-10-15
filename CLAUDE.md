# Qrec

Wayland screen + camera recorder with a built-in editor. Rust workspace,
FCIS architecture. `SPEC.md` is the product contract;
`docs/ARCHITECTURE.md` is the engineering contract (types, protocols,
module boundaries). Read the relevant sections before changing behavior.

## Commands

Everything builds inside the Nix dev shell (GStreamer, GTK and rustc are
not installed system-wide):

```sh
nix develop                      # or: direnv allow (uses .envrc)
cargo build                      # the `qrec` binary
cargo test -p qrec-core          # pure core: unit + proptest + insta
cargo test --workspace           # everything (CLI tests spawn ffmpeg)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo insta review               # accept/reject snapshot changes
```

## Rules

- `qrec-core` is pure: no fs, processes, env, threads, clocks or randomness.
  Time, entropy and confirmation arrive in `Ctx`; I/O is requested with
  `Effect`s and answered with `Event`s. `crates/qrec-core/clippy.toml`
  enforces this; never silence it.
- Every state change goes through `Session::dispatch`. The CLI, IPC and
  GUI must not mutate state any other way (SPEC §4.2).
- Media files are never modified or deleted except by a confirmed purge.
  Exports and frames get new names; never overwrite.
- Every child process logs to a file and never leaves a pipe unread
  (docs/RECORDING.md pitfalls). Every wait has a timeout.
- Don't initialize GStreamer for commands that only need ffmpeg. Never let
  VA-API choose its own device: NVIDIA's renderD128 hangs on the dev machine
  (ARCHITECTURE §8.7).
- `qrec-gui` (GTK views) follows the pen.dev designs in `design/qrec.pen`,
  approved by the product owner on 2026-10-15 (SPEC §15.1). The Swamp Night
  slice is the reference; other themes are token swaps (`design/tokens/`).
- Scratch work (manual checks, recordings, experiments) goes in the
  gitignored `tmp/` at the repo root, never in `/tmp` paths others share.
- JSON is camelCase; error codes are snake_case; CLI `--json` output is
  exactly one object (or JSON lines for streaming commands).
- Tests: planners get `insta` snapshots, invariants get `proptest`
  properties, CLI behavior gets integration tests with ffmpeg fixtures.
