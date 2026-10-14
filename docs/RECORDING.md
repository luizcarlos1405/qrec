# Recording — how it works

This document explains how screen recording works in the sibling project
`../qrec-tui` (Rust TUI, commit `a96258d`). It is meant as the reference for
building recording into this project. Every command and behavior below was
checked on this machine (NixOS, niri on Wayland) on 2026-10-14.

## TL;DR

qrec-tui does not capture the screen itself. It **spawns `wf-recorder` as a
child process, then stops it with `SIGINT`** (the same as pressing Ctrl+C).
`wf-recorder` grabs frames from the compositor through the
`wlr-screencopy` Wayland protocol, which niri supports. It also grabs audio
from PipeWire through its PulseAudio compatibility layer and encodes both with
the ffmpeg libraries into an `.mp4`. Everything else is post-processing done
with plain CLI tools: `ffprobe` (duration), `ffmpeg` (concat, trim, silence
detection) and `mpv` (preview).

```sh
# what qrec-tui effectively runs (with the audio flag corrected, see Pitfall 1)
wf-recorder --output DP-4 --audio=<pulse-source-name> --file chunk-7.mp4
# ...later, to stop:
kill -INT <pid>; wait <pid>
```

## Runtime stack

| Layer | Tool | Notes |
|---|---|---|
| Screen capture | `wf-recorder` 0.6.0 | Uses `wlr-screencopy`. Works on wlroots compositors and niri. Does **not** work on GNOME or KDE, which need xdg-desktop-portal/PipeWire screencast instead. |
| Audio capture | PipeWire (`pipewire-pulse`) | wf-recorder uses its PulseAudio backend by default (`--audio-backend=pipewire` also exists). |
| Device discovery | `wf-recorder -L`, `pactl list short sources` | |
| Encoding | libavcodec, linked into wf-recorder | Defaults to libx264 + AAC |
| Probing / editing | `ffprobe`, `ffmpeg` | |
| Preview | `mpv` | |

Nix dev shell used by qrec-tui (`../qrec-tui/shell.nix`):

```nix
{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  buildInputs = with pkgs; [ rustc cargo rustfmt clippy wf-recorder ffmpeg mpv pulseaudio ];
}
```

(`pulseaudio` is there only for the `pactl` client. The sound server on this
machine is PipeWire.) All of these tools are also installed system-wide here:
`/run/current-system/sw/bin/{wf-recorder,ffmpeg,ffprobe,mpv,pactl,wpctl}`.

## Recording lifecycle in qrec-tui

Relevant code:

- `src/command.rs`: pure builders that return `Command { program, args }` data
- `src/effects.rs`: process spawning, signals and ffprobe
- `src/main.rs`: owns the `Child` handle and the `Instant` start time
- `src/app.rs`: state machine (`Ready → Recording → Ready`)

### 1. Dependency check (startup)

`effects::check_dependencies()` looks up `wf-recorder`, `ffmpeg`, `ffprobe`,
`mpv` and `pactl` on `$PATH`. It exits with a list of whatever is missing.

### 2. Screen discovery

Runs `wf-recorder -L` and parses **stdout + stderr combined**. The output on
this machine:

```
1. Name: DP-3 Description: LG Electronics - LG ULTRAWIDE - DP-3
2. Name: DP-4 Description: GIGA-BYTE TECHNOLOGY CO., LTD. - GS27QA - DP-4
3. Name: eDP-1 Description: BOE - 0x0CC0 - eDP-1
```

`extract_output_name()` strips `N.`, then `Name:`, and cuts at
` Description:`. The result is `DP-4`. The full line is kept as a UI label,
but **only the bare name may be passed to `--output`**. Passing the whole line
makes wf-recorder exit with code 1 (fixed in commit `1a58f00`).

`niri msg outputs` gives the same names plus resolution, scale and position,
if the new project wants richer info.

### 3. Microphone / audio-source discovery

Runs `pactl list short sources` and takes column 2 (the source name):

```
72     alsa_input.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Mic__source   PipeWire  s32le 2ch 48000Hz IDLE
57825  alsa_input.usb-Blue_Microphones_Yeti_Nano_...analog-stereo      PipeWire  s24le 2ch 48000Hz IDLE
57824  alsa_output.usb-Blue_Microphones_Yeti_Nano_...analog-stereo.monitor ...
```

- `alsa_input.*` entries are real microphones.
- `*.monitor` entries are loopbacks of output sinks. Recording one captures
  **system audio** (what is playing through that device), not a microphone.
- qrec-tui puts a synthetic `"No microphone"` entry at index 0. Choosing it
  omits the audio flag, so the file has no audio track.
- `pactl get-default-source` returns the current default source. On this
  machine that is the Yeti Nano input.

### 4. Start recording

1. The core picks the filename `chunk-{next_chunk_number}.mp4`, increments the
   counter and saves `qrec.json` **before** spawning. Numbers are never reused,
   so wf-recorder never runs into an existing file. If it did, it would ask
   for confirmation interactively unless `-y` is passed.
2. `command::wf_recorder_command(output, mic, file)` builds:
   `wf-recorder --output <name> --file <file> [--audio <source>]`
3. `effects::start_recording` runs `std::process::Command::spawn()` with
   stdout and stderr **piped** and logs the PID to `logs.txt`.
4. The shell stores the `Child` and `Instant::now()`. The UI computes elapsed
   time from that `Instant` every ~100 ms tick. It never looks at the file
   while recording.
5. While recording, only `r` (stop) and `q` (stop + quit) are accepted.

### 5. Stop recording

```rust
// effects::stop_recording
unsafe { libc::kill(pid as i32, libc::SIGINT); }
child.wait()?;            // blocks until wf-recorder has flushed and closed the file
```

`SIGINT` is the only clean way to stop wf-recorder. On SIGINT it drains the
encoder and writes the MP4 trailer. The `moov` index is written when the file
is closed, so a `SIGKILL`ed recording is normally unplayable. The recorder
exits with status 0.

After `wait()`:

- If the file does not exist, the app emits `RecordingFileMissing`. No phantom
  chunk is added, and the user is pointed to `logs.txt`.
- Otherwise the duration comes from
  `ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 <file>`
  and the chunk `{id, file, duration_secs, recorded_at}` is appended to
  `qrec.json`.

`q` while recording and app exit both go through the same `stop_recording`,
so a chunk is never left unfinalized.

### 6. Post-processing (after recording)

These steps don't capture anything, but they explain the format choices:

- **Concat / export**: ffmpeg concat demuxer with stream copy,
  `ffmpeg -f concat -safe 0 -i list.txt -c copy output.mp4`. The list uses
  absolute paths: `file '/abs/chunk-1.mp4'`. Stream copy only works because
  every chunk comes from the same wf-recorder settings (same codec,
  resolution and pixel format). **Recording different outputs (resolutions)
  or switching encoders between chunks breaks `-c copy` concat.**
- **Audio delay**: if non-zero, export re-encodes only the audio, using
  `adelay=<ms>|<ms>` (positive) or `atrim=<s>,asetpts=PTS-STARTPTS`
  (negative). Preview passes `mpv --audio-delay=<s>`. The feature was added
  "to compensate for wf-recorder A/V desync" (commit `b9a73a2`).
- **Autotrim**: `ffmpeg -i f -af silencedetect=noise=<dB>dB:d=0.1 -f null -`
  finds leading and trailing silence. Each chunk is cut with
  `ffmpeg -ss <a> -to <b> -i f -c:v libx264 -c:a aac` and then concatenated.

## What wf-recorder produces by default (verified here)

I ran a 3 s test with
`wf-recorder --output eDP-1 --audio=<yeti> --file t.mp4` and stopped it with
SIGINT. The log and `ffprobe` showed:

| Property | Value |
|---|---|
| Container | MP4 |
| Video codec | libx264, `crf=20 preset=superfast tune=zerolatency`, `bframes=0` |
| Pixel format | `yuv420p` full range (`ffprobe` reports `yuvj420p`) |
| Resolution | Native output size (1920x1200 for eDP-1). Scaled outputs are captured at their logical position. |
| Frame rate | **Variable.** `r_frame_rate=60/1` but `avg_frame_rate≈58.3`. On a static screen it fell to ~2 fps. |
| Audio | AAC LC, 48 kHz, stereo, 128 kb/s |
| Start times | video `0.007`, audio `0.000` |
| Exit code on SIGINT | `0` |

Frame rate is variable because of **damage tracking**: wf-recorder only asks
the compositor for a new frame when the screen changes. That keeps files
small, but players and editors may not handle the variable frame rate well.
`-D/--no-damage` records frames continuously, and `-r <fps>` forces a constant
frame rate.

### Hardware encoding (optional)

This machine has two render nodes:

| Node | Vendor | VAAPI result |
|---|---|---|
| `/dev/dri/renderD129` | Intel (`0x8086`), the GPU niri runs on | ✅ `-c h264_vaapi -d /dev/dri/renderD129` works with zero-copy DMA-BUF capture |
| `/dev/dri/renderD128` | NVIDIA (`0x10de`) | ❌ wf-recorder **hung after "selected region" and ignored SIGINT**. It had to be `SIGKILL`ed. |

The default (libx264 on the CPU) is what qrec-tui uses, and it works fine.
If the new project offers GPU encoding, pin the Intel node explicitly. Never
let wf-recorder pick the device on its own.

## Pitfalls found in qrec-tui (fix these in the new project)

1. **The microphone selection is silently ignored.** wf-recorder declares
   `-a, --audio[=DEVICE]` with an *optional* argument (getopt semantics), so
   `--audio <device>` with a space means "audio on, default device" and the
   device name becomes a stray positional argument. Verified: that form logs
   `Using PulseAudio device: default`, while `--audio=<device>` logs
   `Using PulseAudio device: alsa_input.usb-Blue_Microphones_...`. qrec-tui
   only seemed to work because the Yeti is the default source.
   **Always use `--audio=<device>` (or `-a<device>`).**
2. **Piped stdout/stderr are never read.** wf-recorder logs to stderr. If a
   long session ever fills the ~64 KiB pipe buffer, the recorder blocks on
   write and stalls. Redirect stderr to a log file (or `Stdio::null()`), or
   drain it from a thread. Writing it to a log is also the best way to debug
   failures.
3. **`child.wait()` has no timeout.** If wf-recorder ignores SIGINT (see the
   NVIDIA VAAPI hang above), the whole UI freezes. Use SIGINT, then poll
   `try_wait()` for a grace period (e.g. 5–10 s), then `SIGKILL` and report
   the chunk as broken.
4. **Startup failures are only noticed at stop time.** A bad `--output` name
   or audio device makes wf-recorder exit within milliseconds, but the app
   still shows "Recording..." until the user presses stop. Check
   `try_wait()` a few hundred ms after spawning, or watch stderr for
   `selected region`, and surface the error right away.
5. **Use `-y` or unique filenames.** Without `-y`, wf-recorder asks on stdin
   before overwriting. In a raw-mode TUI that looks like a hang.
6. **Very short recordings.** Stopping within a fraction of a second can
   produce a file with ~0 duration or no frames. qrec-tui treats a duration of
   0.0 as valid. Reject or warn instead.
7. **Wall-clock vs. file duration.** The UI timer comes from `Instant`, which
   includes startup latency. Use the `ffprobe` duration as the source of
   truth.

## Recommended recipe for this project

```sh
# OUTPUT: bare name from `wf-recorder -L` / `niri msg outputs`
# SOURCE: name from `pactl list short sources` ('=' is mandatory; omit --audio for no audio)
wf-recorder --output "$OUTPUT" --audio="$SOURCE" --file "$FILE" -y
# optional: -r 60 (constant fps) | -c h264_vaapi -d /dev/dri/renderD129 (Intel GPU)
```

Process handling (Rust sketch, same approach as qrec-tui plus the fixes):

```rust
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn start(output: &str, source: Option<&str>, file: &str, log: std::fs::File) -> std::io::Result<Child> {
    let mut cmd = Command::new("wf-recorder");
    cmd.args(["--output", output, "--file", file, "-y"]);
    if let Some(s) = source {
        cmd.arg(format!("--audio={s}"));
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log)) // never leave a pipe unread
        .spawn()
}

fn stop(child: &mut Child) -> std::io::Result<bool /* clean */> {
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.kill()?; // SIGKILL: file is most likely unusable
    child.wait()?;
    Ok(false)
}
```

Then call `ffprobe` for the duration, and reject missing or 0-length files.

## Manual smoke test

```sh
wf-recorder -L                              # list outputs
pactl list short sources                    # list audio sources
wf-recorder --output eDP-1 --audio="$(pactl get-default-source)" --file /tmp/t.mp4 -y &
sleep 3; kill -INT $!; wait $!; echo "exit=$?"   # expect exit=0
ffprobe -v error -show_entries stream=codec_name,avg_frame_rate,duration -of compact /tmp/t.mp4
```
