//! S17's failing ffmpeg (docs/GUI.md G§7 WP7): a `PATH` whose `ffmpeg`
//! fails the render of the design export's third segment (c1b2, take 3
//! from its first frame: the only unit whose first input is take 3's with
//! no `-ss` before it) with a VA-API-style stderr (BRIEF S17's tail, plus
//! the render node it tried), and runs the real ffmpeg for everything else
//! (the other segments, probes, thumbnails).

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
    let wrapper = bin.join("ffmpeg");
    let script = format!(
        "#!/bin/sh\n\
         # S17: take 3's first render unit fails like a broken VA-API driver.\n\
         case \" $* \" in\n  \
           *' -progress pipe:1 -i file:takes/take-0003/'*)\n    \
             cat >&2 <<'TAIL'\n{TAIL}\nTAIL\n    \
             exit 1 ;;\n\
         esac\n\
         exec '{}' \"$@\"\n",
        real_ffmpeg().display()
    );
    fs::write(&wrapper, script).expect("the wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).expect("a PATH")
}
