# Changelog

## Unreleased

### Changed

- Qrec is now under the GNU General Public License, version 3 or later
  (GPL-3.0-or-later), instead of the MIT license. Releases up to 0.2.1
  stay under the MIT license.

## 0.2.1 (2026-11-17)

A fix for the saved scenes' menu.

### Get it

- **The AppImage**, `Qrec-0.2.1-x86_64.AppImage`: `chmod +x` it and run
  it, as before. The `.sha256` file is its checksum, and `.build.txt`
  lists what it was built from.
- **Nix**: `nix run github:luizcarlos1405/qrec/v0.2.1`, or
  `nix profile install github:luizcarlos1405/qrec/v0.2.1`. In a NixOS
  flake, the input is `qrec.url = "github:luizcarlos1405/qrec/v0.2.1";`.

### Fixed

- A saved scene's ⋮ in the scene menu opened empty. It lists Rename…,
  Replace with current scene, Duplicate and Delete… again. Delete… and
  Replace close the menu and ask first; Duplicate keeps the menu open, the
  copy under the original.
- The Add layer and Add audio track menus say "No sources found" when
  nothing was found, instead of opening empty.

## 0.2.0 (2026-11-16)

Three new autocut options (issue #15), and the autocut page shows its
analysis as it runs.

### Get it

- **The AppImage**, `Qrec-0.2.0-x86_64.AppImage`: `chmod +x` it and run
  it, as with 0.1.0. The `.sha256` file is its checksum, and `.build.txt`
  lists what it was built from.
- **Nix**: `nix run github:luizcarlos1405/qrec/v0.2.0`, or
  `nix profile install github:luizcarlos1405/qrec/v0.2.0`. In a NixOS
  flake, the input is `qrec.url = "github:luizcarlos1405/qrec/v0.2.0";`.

### Before you update

- **Projects move to a new file version.** 0.2.0 opens a 0.1.0 project as
  it is. The first change to it saves the old file as
  `.qrec/qrec.v1.json.bak` and rewrites `qrec.json` as version 2; undo and
  redo keep working across it. From then on Qrec 0.1.0 refuses the project
  ("the project was saved by a newer Qrec"), so update every Qrec you open
  it with.

### New

- **Trim edges only.** Autocut removes only the silence before the first
  sound and after the last, and leaves the pauses in between. It's a
  switch on the autocut page and in Settings › Autocut, and
  `qrec autocut --edges-only`.
- **Keep a previewed cut.** On the autocut page, click a hatched silence on
  the timeline, or uncheck its row, to keep it; Apply cuts the rest. Kept
  cuts belong to that one cut: they aren't saved, and the run after every
  take doesn't use them. `qrec autocut --keep <clip>:<in>-<out>` does the
  same, with the ranges the dry run lists.
- **Silence on several tracks.** The autocut page's "Detect silence on"
  can check several tracks, and autocut cuts only where all of them are
  silent, so a sound on any of them stays. They share one threshold.
  `qrec autocut --track` can be repeated.
- **The autocut page shows its analysis.** When a change has to analyze
  the takes again, the page says how far it got ("Analyzing take 8 ·
  40%"), dims the old plan and takes its hatches off the timeline until
  the new plan is ready.
- The autocut page opens on an empty timeline, so autocut can be set up,
  or set to run after every take, before the first recording.
- Threshold, Min silence, Padding and Min keep explain themselves on
  hover, on the autocut page and in Settings › Autocut.

### Changed

- The timeline hatches what autocut would cut at the project's settings,
  with the padding it keeps beside each cut in blue. After a cut, the lane
  is clean.
- Min keep counts a fragment's sound, not its padding, and its default is
  now 50 ms. A click that split a silence in two no longer stays behind as
  a short clip.
- The picker's New project starts with what the search has, so a project
  the search didn't find can be made without typing its name again.

## 0.1.0 (2026-11-13)

The first release: a screen and camera recorder for Linux with a built-in
editor, made for one loop: press R, talk, press R, repeat.

### Get it

- **The AppImage**, `Qrec-0.1.0-x86_64.AppImage`: one file for x86_64
  Linux. It brings its own GTK, GStreamer, ffmpeg and Mesa, so it runs
  where the system's GTK is older (Ubuntu 24.04, Debian 13, Fedora and Arch
  were tried). `chmod +x` it and run it: it needs neither FUSE nor root.
  The `.sha256` file is its checksum, and `.build.txt` lists what it was
  built from.
- **Nix**, with flakes: `nix run github:luizcarlos1405/qrec/v0.1.0`, or
  `nix profile install github:luizcarlos1405/qrec/v0.1.0`. On NixOS the
  AppImage runs too, through `appimage-run`, but the flake is native there.

### What it does

- Records screens (through the desktop portal on Wayland, `ximagesrc` on
  X11), cameras, microphones and system audio, each layer and track to its
  own file. `R` starts and stops a take and `D` discards the last one;
  `qrec record toggle` does the same from a global shortcut, and
  `qrec doctor` prints the shortcut setup for your desktop.
- Every take lands on a magnetic timeline: trim, split and reorder clips,
  autocut the silences, and undo anything, even after a restart.
- Exports MP4 or WebM (H.264, VP9 or AV1) while you keep editing, saves
  frames as PNG and copies them to the clipboard.
- Saved scenes, a project picker and named project folders.
- The `qrec` command line drives the same project as the window, with
  `--json` for scripts and LLM agents (`qrec agent-guide`).
- `qrec doctor` checks the machine and says what to install.
- Keyboard first, with four themes.

### Known limits

- Only niri has been tested on a real session. GNOME, KDE Plasma, the
  wlroots compositors, Hyprland and X11 desktops should work, but nobody
  has tried them yet: please report what you find.
- Recordings use VA-API on Intel and AMD GPUs, and x264 or OpenH264
  elsewhere. NVIDIA's encoder isn't used, and from the AppImage, a machine
  with only an NVIDIA GPU draws the window in software.
- x86_64 only. There are no distribution packages or Flatpak yet; an AUR
  package and a .deb are next (issue #9).
