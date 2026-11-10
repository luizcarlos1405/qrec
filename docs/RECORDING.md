# Recording — how it works

This document explains how screen recording works in the sibling project
`../qrec-tui` (Rust TUI, commit `a96258d`). It is meant as the reference for
building recording into this project. Every command and behavior below was
checked on this machine (NixOS, niri on Wayland) on 2026-10-14.

The last three sections (X11, the portal backends, the encoders) are about
Qrec's own GStreamer capture (docs/ARCHITECTURE.md A§8.10, A§8.11) away
from niri. Unless a line says otherwise, they were checked here from
2026-10-23 to 2026-11-06 on private servers (an Xvfb, a headless sway with
its own D-Bus, PipeWire and portal), not on the owner's session; what was
read from a project's sources instead of run says so.

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

## X11 (ximagesrc)

On X11 Qrec records a screen as its part of the X server's root window,
with `ximagesrc`: no portal, no picker, no restore token. Adding the screen
layer is the consent. A Wayland session never takes this path, whatever
`DISPLAY` says (`QREC_SCREEN_BACKEND=x11` forces it, with a warning). The
monitors come from RandR (1.5 `GetMonitors`, else the lit CRTCs, else the
whole screen as one), read again for every pipeline that grabs a screen.

```sh
# what the engine builds for VIRT-R (960x1080 at 960,0) on a 30 fps canvas
ximagesrc display-name=:1 startx=960 starty=0 endx=1919 endy=1079 \
    use-damage=false show-pointer=true \
  ! video/x-raw,framerate=30/1 \
  ! capssetter caps="video/x-raw,pixel-aspect-ratio=1/1" \
  ! videoconvert ! video/x-raw,format=I420,colorimetry=bt709 ! tee …
```

Measured on a private Xvfb (GStreamer 1.26.11):

| What | Result |
|---|---|
| 1080p60 | `ximagesrc ! video/x-raw,framerate=60/1 ! videoconvert ! I420 ! fpsdisplaysink sync=false` on a 1920x1080 screen: 154 frames rendered in 2.99 s, **0 dropped**, 60.54 fps on average. XShm (`XShmGetImage`, once per frame) keeps up. |
| Default rate | Without a `framerate` caps filter ximagesrc negotiates BGRx at 25/1. Without the engine's capsfilter, a take's source sent 89 buffers in 3.5 s. |
| CPU, 1080p30 | Recording a whole 1920x1080 screen: the headless host takes 51 % of a core with a quiet screen, 84 % with scrolling bars. The grab and the I420 conversion alone take 15 %. |
| CPU, 1080p60 | 105 % and 175 %. |
| Xvfb | 6–13 % of a core. No fill frames in any of these takes. |

The CPU figures are from an Intel Core Ultra 7 265H, x264 veryfast, a debug
build. Xvfb has no GPU to read the frames back from, so a real X server may
cost more (not measured). By hand, the GUI on a private Xvfb added VIRT-R
from the first-run picker, the live preview showed it at once, and a take
from the Record button had no fill frames.

Pitfalls:

1. **The ends are inclusive.** `endx = x + w - 1`, `endy = y + h - 1`:
   `startx=100 starty=50 endx=1059 endy=589` gave 960x540, and
   `endx=1060 endy=590` gave 961x541.
2. **An odd size is fatal for x264.** At 961x541 x264enc failed with "Can
   not initialize x264 encoder" (I420 has no odd sizes). Qrec rounds the
   width and height down to even and refuses anything under 2x2.
3. **A rectangle outside the screen grabs the whole screen, silently.**
   ximagesrc says so only in its debug log ("User put in co-ordinates
   overshooting the X resolution, setting to full screen"). Qrec checks the
   rectangle against the root window first: a monitor the X server doesn't
   have, or one outside the root, fails the take naming the layer.
4. **XGrabServer on every frame.** ximagesrc wraps each grab in
   `XGrabServer`/`XUngrabServer`, so other X clients wait for the length of
   each grab (read from its source in gst-plugins-good; not measured). With
   XDamage on, its default, it also makes a round trip per changed
   rectangle; Qrec sets `use-damage=false` and grabs the whole part every
   frame period.
5. **Never under XWayland.** XWayland's root window holds only X clients,
   never the desktop, so a Wayland session records through the portal.
6. **Non-square pixels.** ximagesrc derives the pixel aspect ratio from the
   X screen's millimetres: on a 1920x1080 screen of 508x200 mm
   (`xrandr --fbmm 508x200`) a take came out 11:10 and played stretched.
   Its caps are fixed, so a capsfilter can't change that. A `capssetter`
   after the rate's capsfilter does; in front of it, it hides the rate
   (25 fps, not-negotiated).
7. **Monitor changes mid-take.** The rectangle is fixed. When the root
   shrinks under it (a monitor unplugged or turned off), ximagesrc grabs
   the whole new root instead: a take of VIRT-R then recorded VIRT-L's
   red, or green frames, and still ended ok. A monitor that goes away or
   moves while the rectangle still fits (an external monitor on the left
   unplugged, the laptop panel moving to 0,0) goes unnoticed altogether.
   Qrec stops the branch in both cases, as a portal cast ends when its
   output goes: a guard on ximagesrc's pad sees its rectangle rewritten,
   and a RandR watch (the root's `ConfigureNotify`, RandR's screen, CRTC
   and output notifications) sees the monitor go away, move or change
   size. The layer's stream is lost with the reason in `recorder.log`, and
   no frame of the other monitor reaches the file: in the tests, both
   takes that lost their only layer this way landed broken with L1
   missing. In Live mode the layer goes dead; the next live scene, take or
   retry rebuilds it from the monitors as they are then.
8. **An X server that dies or hangs.** A dead one makes Xlib exit the whole
   process (its default IO error handler), headless host included, and the
   take is left to the startup recovery. A hung one loses the stream after
   `STALL_LIMIT` (3 s), but the teardown then waits on ximagesrc and the
   engine reports itself stuck.
9. **Output names differ from Wayland's.** An X server's RandR outputs are
   named by its driver: Xorg's modesetting driver calls the kernel's
   `HDMI-A-1` `HDMI-1`, and amdgpu counts from 0 (`DisplayPort-0`,
   `HDMI-A-0`). A screen layer added under Wayland may not find its monitor
   under X11 on the same machine, and the other way round (read from the
   drivers' sources; not checked on hardware).

## Portal backends

Under Wayland every screen goes through the ScreenCast portal: one session
per screen layer, `persist_mode = 2`, a restore token per output in
`portal-tokens.json`. Which monitor a stream shows is read from what the
backend sends with it (A§8.11): the screen its mapping id names, else the
one with its position and size, else the one screen of its size. A token
is filed only under the monitor the stream was identified as, never under
a guess:
xdg-desktop-portal 1.20.4 hands the same token back on a restore (checked),
so a token filed under the wrong output would restore that monitor on every
take without ever showing the dialog.

| Backend (desktops) | Restore tokens | Mapping id | Position, size | Qrec identifies by | Checked |
|---|---|---|---|---|---|
| xdg-desktop-portal-gnome (niri, GNOME) | Kept | None. GNOME sends a UUID, in remote-desktop sessions only. | Real logical position and size | Position and size | niri: the dev machine's own takes, on the owner's session (a second take's restore with today's matching is still the owner's manual check); GNOME not run, from xdg-desktop-portal-gnome's and mutter's sources |
| -kde (KDE Plasma) | Kept (from its source) | The output's name | Its position and size | Mapping id | Not run, from its source |
| -wlr (sway, river, wayfire, labwc) | Only when the portal runs with `XDPW_PERSIST_MODE` (`transient` or `permanent`; Qrec's hint says `permanent`); otherwise the chooser shows on every take. The chooser is a program it runs (slurp, wofi, rofi or fuzzel); without one it picks any output. | Only on its main branch; none in the 0.8.2, 0.8.3 and 0.8.5 releases | Real xdg-output positions from 0.7.1 to 0.8.x; 0.7.0 and older (Debian 12, Ubuntu 22.04) report a placeholder position. Logical sizes, which sway floors (1706x960 at 1.5, 1092x614 at 1.25). | Mapping id on main; position and size on 0.8.x. On 0.7.0 and older two monitors of one size can still be confused. | A private headless sway, xdg-desktop-portal 1.20.4, with xdpw main and 0.8.2 and a scripted chooser; 0.7.0 not run |
| -hyprland (Hyprland) | Only when the person ticks the restore-token box in its picker | The output's name from 1.4.0; none in the 1.3 line most distributions ship | 0,0 for every monitor (a placeholder); the buffer's pixels, in the panel's own orientation (a rotated monitor's sides swapped) | Mapping id; before 1.4.0, a size no other screen has, either way round. Two monitors of one size are shared unverified, never filed under each other. | Not run: from xdph's sources (1.3.9, 1.4.0) |
| -cosmic (COSMIC) | Kept, by output name (from its source) | The output's name | Logical position; the buffer's pixels (the current mode) | Mapping id | Not run, from its source |

Notes:

- **Names.** A mapping id, a restore token's key and a screen layer's
  output all name a monitor the way `wl_output` (or xdg-output) does, as
  `sources list` shows it (`DP-4`, `eDP-1` on the dev machine). An X server
  names the same monitors after its driver (X11 pitfall 9), so a name may
  not carry over between an X11 and a Wayland session.
- **A stream nothing identifies** (two monitors at one place, no position
  and a size two screens share, no screens discovered) is still recorded
  for that take, with a warning in the log, but its token isn't kept and
  the requested output's goes: the dialog shows again next time.
- **Fractional scales.** Compositors round, floor or ceil a logical size
  against the scale, so Qrec takes each screen's scale from xdg-output's
  logical size and matches a stream's logical size within a pixel on each
  side. On the private sway, outputs at 1.5 and 1.25 restored by position
  and size, and `sources list` gave a 2560x1600 output rotated 90° as
  1600x2560 at 1.25 and a 1366x768 one as 1.2509 (the ratio sway's floored
  1092x614 leaves), as swaymsg reports them.
- **wl_output alone isn't enough off niri.** wlroots sends 0,0 as every
  output's wl_output position, sway sends a whole-number scale (2 for 1.25
  and for 1.5), and a wl_output below v4 (the sway of Debian 12 and Ubuntu
  22.04, older KWin and Mutter) has no name. xdg-output gives the logical
  position and size, and from v2 the name.
- **No portal on X11.** KDE's portal registers ScreenCast in an X11 session
  and then refuses to share ("screen sharing is not available in X11
  sessions", from its source), and Xfce, Cinnamon and MATE ship no
  ScreenCast backend (from their portals' sources). X11 screens are
  ximagesrc's (above), and the doctor never asks the portal there.

## Encoders off the dev machine

Qrec records with the render node's VA encoder, else x264enc, else
openh264enc (A§8.11), and exports MP4 with h264_vaapi on a verified node,
else libx264, else libopenh264 (A§9.3).

- **Low-power VA encoders.** Intel Arc, Meteor Lake and newer expose only
  the low-power (VDENC) H.264 entrypoint, whose GStreamer element is
  `va*h264lpenc`. Qrec records with the node's `va*h264enc`, else its
  `va*h264lpenc`, both CQP 18. Not checked on hardware: this machine's iHD
  has the full encoder, so whether `vah264lpenc` takes `rate-control=cqp`
  there is unverified.
- **i965.** Intel's older VA driver drives the chips iHD doesn't (G45 to
  Haswell and Bay Trail, and the Cherry View and Braswell Atoms), and Qrec
  loads it there. GStreamer's VA plugin registers no encoder with i965 (it
  is blocklisted for encoding unless `GST_VA_ALL_DRIVERS` is set), so those
  machines record with x264 or OpenH264 on the CPU. ffmpeg's `h264_vaapi`
  does encode with i965, so MP4 exports still can. On a Broadwell to Comet
  Lake whose iHD encodes nothing (Debian's and Ubuntu's free build) and
  whose i965 does, exports load i965 there, while recordings keep the
  node's iHD. None of this ran on such a chip: this machine's Intel GPU
  (Arrow Lake) uses iHD, and the rules are tested on sysfs and driver
  fixtures.
- **NVIDIA.** NVIDIA has no VA-API encoder, and VA-API on this machine's
  NVIDIA node hangs (see "Hardware encoding" above). Qrec never VA-probes
  an NVIDIA node, configured or not: there, recordings and exports encode
  on the CPU.
- **OpenH264's error line at the end of a take.** Where openh264enc
  records, each encoder prints one `Error:CWelsH264SVCEncoder::EncodeFrame(),
  cmInitParaError.` at EOS: GStreamer's drain calls `EncodeFrame` without a
  picture. It is harmless and the files are complete, but it lands in the
  host's stderr and log.
- **OpenH264's largest frame** is 9,437,184 pixels (36,864 macroblocks:
  4096x2304, whatever the shape). A larger one fails when the stream
  starts, with a bare not-negotiated error, so Qrec refuses such a screen,
  camera mode or MP4 canvas before it starts.
