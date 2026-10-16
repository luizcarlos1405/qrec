//! S17's failing ffmpeg (docs/GUI.md G§7 WP7, G§8.4): a `PATH` whose
//! `ffmpeg` lets the design export's first two render units finish and
//! fails the third with BRIEF S17's stderr, so the panel fails where the
//! export got to.
//!
//! - Units 1 and 2 (c7f3 and c8a1, take 1's: its only units) are the real
//!   ffmpeg.
//! - Unit 3 (c1b2: take 3 from its first frame, the only unit whose first
//!   input is take 3's with no `-ss` before it) waits until units 1 and 2
//!   are in the segment cache (`.qrec/cache/segments/*.mov`; the renders
//!   run in the project folder) and 2 s more (the panel reads "Rendering
//!   clip 3/10" meanwhile), then writes the tail (plus the render node it
//!   tried, which the hint reads), marks `failed` and exits 1.
//! - Every other unit waits for that mark, then sleeps until the export
//!   stops it (the shell stops the others when one fails), so no frame of
//!   theirs counts: the export fails at units 1 and 2's 867 of 7,391
//!   frames, 11 %.
//! - Anything else (probes, thumbnails, peaks) is the real ffmpeg.
//!
//! Each wait has a bound (50 s, then it goes on as if it had happened);
//! the shell's 60 s stall watchdog is the backstop.

#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// BRIEF S17's stderr tail, with the node line the hint reads.
pub const TAIL: &str = "ffmpeg version 7.1 Copyright (c) 2000-2024 the FFmpeg developers
Input #0, matroska,webm, from 'takes/take-0003/L1-screen-DP-4.mkv':
  Duration: 00:00:45.90, start: 0.000000, bitrate: 11982 kb/s
  Stream #0:0: Video: h264 (High), yuv420p(tv, bt709, progressive), 2560x1440, 30 fps
Input #1, matroska,webm, from 'takes/take-0003/L2-camera-0.mkv':
  Stream #1:0: Video: h264 (High), yuv420p(tv, bt709, progressive), 1280x720, 30 fps
[AVHWDeviceContext @ 0x5581c3e0a2c0] Trying to use DRM render node for device /dev/dri/renderD129.
[AVHWDeviceContext @ 0x5581c3e0a2c0] Failed to initialise VAAPI connection: -1 (unknown libva error).
Device creation failed: -5.
[vf#0:0 @ 0x5581c3dfa840] Error initializing filter 'hwupload' with args 'derive_device=vaapi'
Error opening output file .qrec/cache/segments/9f3c2a71e4.mkv.
Error opening output files: Input/output error";

/// The frames units 1 and 2 render (c7f3's 410 and c8a1's 457), and so
/// the percent the export fails at: ⌊0.9 · 867⌋ = 780 of 7,391, 11 %.
pub const RENDERED_FRAMES: u64 = 867;
pub const FAILED_PERCENT: u64 = 11;

/// The ffmpeg on `PATH`.
pub fn real_ffmpeg() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("ffmpeg"))
        .find(|p| p.is_file())
        .expect("ffmpeg on PATH")
}

/// Writes the wrapper into `<root>/fail-ffmpeg/` and returns a `PATH`
/// with it first.
pub fn failing_path(root: &Path) -> OsString {
    let bin = root.join("fail-ffmpeg");
    fs::create_dir_all(&bin).expect("the wrapper's folder");
    let mark = bin.join("failed");
    let _ = fs::remove_file(&mark);
    let wrapper = bin.join("ffmpeg");
    let real = real_ffmpeg();
    let script = format!(
        "#!/bin/sh\n\
         # S17 (G§7 WP7): units 1 and 2 render, unit 3 fails once they are\n\
         # cached, every later unit waits until it failed.\n\
         mark='{mark}'\n\
         case \" $* \" in\n  \
           *' -progress pipe:1 '*'-i file:takes/take-0001/'*)\n    \
             exec '{real}' \"$@\" ;;\n  \
           *' -progress pipe:1 -i file:takes/take-0003/'*)\n    \
             i=0\n    \
             while [ \"$(ls .qrec/cache/segments 2>/dev/null | grep -c '\\.mov$')\" -lt 2 ] && [ $i -lt 500 ]; do\n      \
               sleep 0.1; i=$((i + 1))\n    \
             done\n    \
             sleep 2\n    \
             cat >&2 <<'TAIL'\n{TAIL}\nTAIL\n    \
             : > \"$mark\"\n    \
             exit 1 ;;\n  \
           *' -progress pipe:1 '*)\n    \
             i=0\n    \
             while [ ! -e \"$mark\" ] && [ $i -lt 500 ]; do\n      \
               sleep 0.1; i=$((i + 1))\n    \
             done\n    \
             sleep 30\n    \
             exit 1 ;;\n\
         esac\n\
         exec '{real}' \"$@\"\n",
        mark = mark.display(),
        real = real.display(),
    );
    fs::write(&wrapper, script).expect("the wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).expect("a PATH")
}
