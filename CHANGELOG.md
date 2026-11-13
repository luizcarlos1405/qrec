# Changelog

## Unreleased

### Changed

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
