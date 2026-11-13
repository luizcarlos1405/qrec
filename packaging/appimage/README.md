# The Qrec AppImage

One file that runs Qrec on x86_64 Linux distributions whatever their GTK:
Ubuntu 24.04, Debian 13, Fedora 45 and Arch were tried (below). It is made the
way pkgforge-dev's [Anylinux AppImages] are: Qrec is built in an Arch Linux
container against Arch's packages, then [quick-sharun] bundles it with every
library it loads, dlopened ones included, glibc and Mesa among them. From the
host it takes only the kernel, the session (display, portals, PipeWire or
PulseAudio, D-Bus) and the NVIDIA driver, if there is one. It needs neither
FUSE nor user namespaces: without FUSE it extracts itself and runs.

[Anylinux AppImages]: https://pkgforge-dev.github.io/Anylinux-AppImages/HOW-TO-MAKE-THESE.html
[quick-sharun]: https://github.com/pkgforge-dev/Anylinux-AppImages/blob/main/useful-tools/quick-sharun.sh

## Build it

```sh
packaging/appimage/build.sh              # tmp/appimage/out/Qrec-<version>-x86_64.AppImage
packaging/appimage/build.sh --builder    # only (re)make the builder image
```

The host needs `docker`, nothing else (no Nix shell). The first run makes the
builder image, `qrec-appimage-builder:2026-11-09-r2` (about 3.2 GB): Arch
Linux with the packages of one day of the Arch Linux Archive and pkgforge-dev's
debloated Mesa and glycin-ng. Then each build:

1. compiles `qrec` (`cargo build --release --locked`) from this checkout,
   mounted read-only, and installs it to the container's `/usr` with its desktop
   entry and icons (`data/install.sh`) and the GUI's fonts
   (`/usr/share/qrec/fonts`, downloaded and checked by hash);
2. deletes the GStreamer plugins Qrec must not load: `nvcodec` (the only one
   of the four GPU probers Arch installs with gst-plugins-bad; `qsv`, `msdk` and
   `hip` are packages of their own, left out) and `python` (Arch's GES pulls in
   gst-python, which would start a Python without its standard library in every
   registry scan);
3. runs quick-sharun on `qrec`, `ffmpeg`, `ffprobe`, `pactl` and `wl-copy`,
   whose dlopen survey runs the window on a test-pattern project under Xvfb for
   25 s;
4. puts the unpatched `qrec` back, adds the Adwaita icon theme and Qrec's
   environment (below), makes the AppImage (DwarFS, zstd) and lets its runtime
   extract images of any size when FUSE is missing (`URUNTIME_EXTRACT=2`).

Containers run at `nice 19` on at most `JOBS` CPUs (default 3). Everything goes
under `tmp/appimage/` (`QREC_APPIMAGE_DIR`), owned by you: `out/` (the
AppImage, `Qrec-<version>-<commit>-x86_64.AppImage`, or
`Qrec-<version>-x86_64.AppImage` when built at the tag `v<version>`; its
`.sha256`; and `.build.txt` with every package version), `build/AppDir`, the
Cargo cache (`cargo-home/`, `target/`) and `cache/` (fonts, quick-sharun). With
the builder image and the Cargo cache in place, a build takes about 5 minutes,
half of it DwarFS's compression; making the builder image downloads about 460
MB from the archive (2 GB installed), and a cold `cargo build` adds about 4
minutes. `ARCH_SNAPSHOT=YYYY/MM/DD` picks another archive day (and makes a new
builder image).

## Try it

```sh
packaging/appimage/test.sh                    # the newest AppImage, the four images
packaging/appimage/test.sh tmp/appimage/out/Qrec-….AppImage fedora:latest
QREC_TEST_VA=1 packaging/appimage/test.sh     # also VA-API on this machine's Intel or AMD GPU
```

Each container (`ubuntu:24.04`, `debian:13`, `fedora:latest`,
`archlinux:latest` by default) gets the AppImage read-only and no `/dev/fuse`,
installs only Xvfb and D-Bus, and runs `--version` and `doctor` (with the
real backend: without a display, screens are off and everything else must
come from the bundle; the report must name the AppImage and no package
manager), then with the fake capture backend: `sources list`; `init`, a
scene, a take and an MP4 export checked with the bundled `ffprobe`; a take
started by one AppImage run and stopped by another; the window on a private
Xvfb, with a screenshot, the renderer GTK picked (Cairo without a render
node, GL with one) and the GL context it made; and
`--appimage-extract-and-run`: 19 checks. Results go to
`tmp/appimage/test/<image>/summary.txt` (with Qrec's `gpu:` log lines),
screenshots to `tmp/appimage/shots/<image>.png`. Nothing uses the host's
display, session bus or FUSE. With `QREC_TEST_VA=1` each container also gets
the host's first Intel or AMD render node, never an NVIDIA one, and no other
device.

On a kernel that hands AppImages to an interpreter of the host's through
binfmt_misc (NixOS's `programs.appimage.binfmt`, which runs `appimage-run`), a
container can't start one: that interpreter isn't in it. `test.sh` then tests a
copy whose AppImage magic (ELF ident bytes 8–10, `AI\2`) is zeroed, which the
kernel runs itself. uruntime doesn't read those bytes, so the copy behaves as
the AppImage does on a host without that handler.

### Tried on (2026-11-10)

| Image | glibc | Result |
| --- | --- | --- |
| `ubuntu:24.04` (24.04.5) | 2.39 | all 19 checks pass |
| `debian:13` | 2.41 | all 19 checks pass |
| `fedora:latest` (45) | 2.44 | all 19 checks pass |
| `archlinux:latest` | 2.44 | all 19 checks pass |

None of them had GTK, GStreamer, libadwaita or ffmpeg installed. The window's
log has one warning on each, that there is no accessibility bus (the containers
run no AT-SPI). A container sees the host's render nodes in sysfs (here an
Intel Arrow Lake iGPU and an NVIDIA GPU, NVIDIA's kernel module loaded) but
gets no device node, so the window drew with Cairo (`GskCairoRenderer`, no GL
context), Qrec's log saying why, and EGL went to the bundled Mesa, Vulkan to
its `anv` (sharun's `VK_DRIVER_FILES`, the bundle's, replaced). The doctor
named the AppImage (`AppImage: /opt/qrec.AppImage
(APPDIR=/tmp/appimage_extracted_…)`), said `/dev/dri/renderD128 can't be
opened: No such file or directory` in its VA-API rows, with how to give a
container the node, and named no package manager. With `QREC_TEST_VA=1` on that
iGPU (`8086:7d51`, i915), the doctor bound GStreamer's `vah264enc` to the node
and ffmpeg's `h264_vaapi` check passed, both on the bundled `iHD`, and an
earlier run had the bundled ffmpeg encode 1080p with `h264_vaapi`; the window
kept GL then, though on softpipe: Xvfb has no DRI3, so Mesa can't draw on the
GPU there. The fake backend records with x264 and exports with libx264
everywhere (Qrec skips VA-API under it), so the takes themselves don't show
the GPU.

A take started by one AppImage run and stopped by another worked on all four,
and in two more cases tried by hand: FUSE mounts, as root and as a user with
the setuid `fusermount3`. The headless host runs from the mount or the
extracted folder and outlives the CLI that started it: uruntime keeps a mount
while a process runs from it, and an extracted folder while its subreaper still
has descendants.

## Pins

| What | Version | From |
| --- | --- | --- |
| Base image | `archlinux:latest` of 2026-11-09, `sha256:d4594bb2…c899` | Docker Hub |
| Arch packages | the Arch Linux Archive of 2026-11-09 | `https://archive.archlinux.org/repos/2026/11/09/` |
| quick-sharun | `pkgforge-dev/Anylinux-AppImages` at `3bc6529` (2026-11-09), sha256 checked | `https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/3bc6529073bf952de8db7b62c86380dbcdb38b64/useful-tools/quick-sharun.sh` |
| sharun, `anylinux.so` | Anylinux-sharun 3.8.3, sha256 pinned by quick-sharun | `https://github.com/pkgforge-dev/Anylinux-sharun/releases/download/3.8.3/sharun+helper-libs-x86_64.tar` |
| appimagetool, uruntime, mkdwarfs | Anylinux-appimagetool 0.5.6, sha256 pinned by quick-sharun | `https://github.com/pkgforge-dev/appimagetool/releases/download/0.5.6/appimagetool-full-x86_64-linux` |
| cross-libc-dlopen | 0.2.10, sha256 pinned by quick-sharun | `https://github.com/pkgforge-dev/cross-libc-dlopen/releases/download/v0.2.10/cross-libc-dlopen-x86_64.tar` |
| Mesa, its Vulkan drivers, glycin-ng | pkgforge-dev's debloated builds (below): Mesa 26.2.6-1 built 2026-11-08 (`mesa-mini` sha256 `8eb0b90d…`), glycin-ng 0.5.3-1 built 2026-11-06 (`aaa865d9…`) | `https://github.com/pkgforge-dev/archlinux-pkgs-debloated/releases/download/continuous/{mesa,vulkan-intel,vulkan-radeon,vulkan-virtio,glycin}-mini-x86_64.pkg.tar.zst` |
| Fraunces | 1.000, the TTF's sha256 checked | `https://github.com/undercasetype/Fraunces/releases/download/1.000/UnderCaseType_Fraunces_1.000.zip` |
| Atkinson Hyperlegible Next | 2.001, commit `7925f50` as in nixpkgs, checked | `https://github.com/googlefonts/atkinson-hyperlegible-next/archive/7925f50f649b3813257faf2f4c0b381011f434f1.tar.gz` |
| JetBrains Mono | 2.304 release build, checked | `https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip` |
| Rust | Arch's 1.99.1, with `Cargo.lock` (`--locked`) | the archive day |

The one input that can't be pinned is pkgforge-dev's debloated packages:
they are published only as a rolling `continuous` release whose files get
replaced. The builder fetches them itself, refuses a Mesa whose version isn't
the snapshot's, and keeps their sha256s and build dates in the image
(`/opt/qrec-appimage/debloated.txt`) and in each AppImage's `.build.txt`.
Fraunces' and Atkinson Hyperlegible Next's files are byte for byte the Nix
package's; JetBrains Mono's is upstream's release build, where nixpkgs builds
the same version from its sources.

## What it holds

From the archive day unless said otherwise:

| Part | Version | Notes |
| --- | --- | --- |
| Qrec | the checkout's `qrec`, release profile | `shared/bin/qrec` |
| glibc, its loader | 2.44 | `ld-linux-x86-64.so.2` loads every bundled binary |
| GTK 4, libadwaita | 4.24.1, 1.10.0 | with the GSettings schemas (and dconf's GIO module), the Adwaita 51 icon theme and cursors, hicolor, XKB data (xkeyboard-config 2.49), X11 locale and compose data, MIME data |
| Image loading | gdk-pixbuf 2.44.8 with glycin-ng 0.5.3 (debloated), librsvg 2.63.4 | gdk-pixbuf 2.44 has no loader modules: glycin-ng decodes the formats, SVG through resvg, in-process, sandboxed with Landlock instead of GNOME glycin's bwrap (no user namespaces) |
| GStreamer | 1.28.8 | core, `gst-plugin-scanner`, base, good, bad (without nvcodec, qsv, msdk, hip), ugly, libav, VA (`gst-plugin-va`), `pipewiresrc` (PipeWire 1.6.10), GES, gst-plugins-rs' `gtk4paintablesink` (0.15.4) |
| ffmpeg, ffprobe | 9.0.2 | Arch's build: libx264, libvpx, libopus, libsvtav1 (4.2.0), the native AAC encoder, VA-API; no libopenh264 (below) |
| pactl | PulseAudio 17.0 | talks to the host's PulseAudio or pipewire-pulse |
| wl-copy | wl-clipboard 2.3.0 | serves a copied image after Qrec quits |
| Mesa | 26.2.6, debloated: no LLVM | EGL/GL (iris, crocus, radeonsi, r600, nouveau, virgl, zink, softpipe), GBM, Vulkan (anv, hasvk, radv, venus), VA-API (`radeonsi`, `r600`, `nouveau`, `virtio_gpu`) |
| VA-API | libva 2.24.1, intel-media-driver 26.3.5 (`iHD`, with its non-free kernels, which encode), libva-intel-driver 2.4.5 (`i965`) | in `lib/dri` |
| libglvnd | 1.7.0 | EGL/GL dispatch |
| Fonts | Fraunces, Atkinson Hyperlegible Next, JetBrains Mono | `share/qrec/fonts`, where the GUI looks (`<exe>/../share/qrec/fonts`), with their OFL texts in `share/licenses/qrec-fonts` |
| Desktop entry, icon | `com.rsensato.qrec.desktop`, its 256 px icon and `.DirIcon` at the root | the hicolor icons in `share/icons` |
| AppStream metadata | `com.rsensato.qrec.metainfo.xml` (`data/`) | `share/metainfo`, linked from `usr/share/metainfo` |

Qrec runs nothing from v4l-utils (cameras come from GStreamer, PipeWire and
sysfs), so it isn't bundled; only the Nix wrapper's `PATH` has it.

## How it starts

The AppImage is uruntime with a DwarFS image. uruntime mounts it with FUSE
when it can (a stable mount point, dropped 5 s after the last process running
from it is gone), else extracts it to `/tmp/appimage_extracted_<hash>`, runs
it from there as a child subreaper that waits for every process started from
it, and deletes it after. In the containers here (3 CPUs), `qrec --version`
took 0.4 s on a fresh mount (as a user with the setuid `fusermount3`), 0.06 s
on one still mounted, and 0.8 s when it extracts; the first run on a machine
scans GStreamer's plugins into the AppImage's registry (about 1 s more).
Then:

1. `AppRun` (sharun) sets `APPDIR` and runs `AppRun.sh`, which sources
   `AppRun.lib` and `bin/*.hook`. `AppRun.lib` points `XDG_CACHE_HOME` at
   `~/.cache/AppImage-Cache`: Qrec's capabilities cache and GStreamer registry
   there are the AppImage's own, apart from an installed Qrec's.
2. `bin/qrec` is sharun again: it reads `.env`, sets the bundle's variables
   (`GST_PLUGIN_*`, `GST_PLUGIN_SCANNER`, `LIBVA_DRIVERS_PATH`,
   `__EGL_VENDOR_LIBRARY_DIRS`, `VK_DRIVER_FILES`, `GIO_MODULE_DIR`,
   `GSETTINGS_SCHEMA_DIR`, `XDG_DATA_DIRS` with `$APPDIR/share` first,
   `PIPEWIRE_MODULE_DIR`, `SPA_PLUGIN_DIR`, `PATH` with `$APPDIR/bin` first, …)
   and starts `shared/bin/qrec` with the bundled loader and `--library-path`,
   in the same process: `/proc/self/exe` stays `$APPDIR/bin/qrec`, so Qrec
   finds its fonts and starts its probe children and headless host from there,
   and finds the bundled `ffmpeg` and `pactl` on `PATH`.
3. `anylinux.so`, preloaded, takes those variables away from the programs
   outside the AppImage Qrec starts (a file manager, say), and keeps glibc's
   NSS to the bundled modules.

What `build.sh` adds (`tune_appdir`):

- `.env`: GStreamer scans the bundled plugins only (`GST_PLUGIN_SYSTEM_PATH_1_0`
  and `GST_PLUGIN_SYSTEM_PATH` set, `GST_PLUGIN_PATH`, `GST_PLUGIN_PATH_1_0` and
  `GST_PLUGIN_SCANNER_1_0` unset), as the flake's wrapper does.
- No startup hook of Qrec's: the prototype's `bin/50-qrec-egl.hook` is gone,
  since Qrec applies its GPU rules from inside the AppImage (A§8.10). It
  knows the AppImage by `$APPDIR` holding its executable, takes the bundle's
  EGL and Vulkan manifests first, and replaces the
  `__EGL_VENDOR_LIBRARY_FILENAMES` and `VK_DRIVER_FILES` that sharun points
  into `$APPDIR`: with an Intel or AMD render node, EGL loads the bundled Mesa
  only, never NVIDIA's driver or the host's Mesa (NixOS's
  `/run/opengl-driver`); with NVIDIA alone, sharun's default puts the host's
  NVIDIA driver first; without a render node it may open, the window draws
  with Cairo (`GDK_DISABLE=gl,vulkan`). Values pointing elsewhere are the
  user's and stay. The window's log says what it set ("gpu: …").
- The AppStream metadata, `share/metainfo/com.rsensato.qrec.metainfo.xml`,
  and the desktop entry it launches, a link to the root's in
  `share/applications`, with `usr/share/metainfo` and `usr/share/applications`
  linking to them: an extracted AppImage then has them where AppImage tools,
  catalogs and `appstreamcli validate-tree` look (it passes). quick-sharun
  copies neither, and pkgforge's appimagetool reads neither (it checks
  `AppRun`, `.DirIcon` and the root's desktop entry), so it says nothing about
  them.
- `qrec` isn't patched. quick-sharun rewrites `/usr/share` and `/usr/lib` in
  every binary that names them, to `/tmp` links into the AppImage; in Qrec
  those are lookups of the host's (`/usr/share/zoneinfo`, the VA driver
  directories, `os-release`).
- The Adwaita icon theme: quick-sharun bundles only hicolor.

## Known limits

- **NVIDIA's driver can't be bundled.** With NVIDIA alone, the window draws
  with the host's NVIDIA EGL (sharun loads it next to the bundled glibc;
  untested here); recordings use x264 on the CPU, since NVIDIA has no VA-API
  encoder and Qrec never probes an NVIDIA node. With an Intel or AMD GPU next
  to it, Qrec uses that one, as natively.
- **No llvmpipe.** The debloated Mesa has no LLVM. Without a render node Qrec
  may open (a VM without 3D, a container), the window draws with GTK's Cairo
  renderer instead, as Qrec's pixman GUI suites do. Where Mesa has a node but
  can't draw on it, GL runs on softpipe, much slower than with a
  distribution's Mesa: NVIDIA-driven screens with the iGPU on, or an X server
  without DRI3 (Xvfb, a VNC server).
- **AMD** gets radeonsi built without LLVM (ACO compiles its shaders) and
  without Mesa's check for newer amdgpu kernels; not tried on AMD hardware.
- **ffmpeg has no libopenh264**: Arch builds it without. MP4 exports use
  h264_vaapi or libx264, which is always there, so Qrec's libopenh264 fallback
  never runs (`qrec doctor` lists it as optional). GStreamer's `openh264enc` is
  bundled.
- **NixOS with `programs.appimage.binfmt`** hands every AppImage to
  `appimage-run`, which unpacks it with `unsquashfs`: a DwarFS AppImage like
  this one doesn't start that way. NixOS has the flake.
- **Size**: 133 MiB (490 MiB unpacked, 4,776 inodes). The biggest parts,
  unpacked: intel-media-driver's `iHD` (40 MiB), Mesa's libgallium (36 MiB),
  ICU's data (32 MiB, for libxml2 and raptor2), the Vulkan drivers (51 MiB),
  x265 (21 MiB, ffmpeg's), libavcodec (20 MiB), Adwaita's icons (15 MiB) and
  GTK (13 MiB).
- The runtime says "failed to utilize FUSE during startup! Trying to extract
  and run..." on stderr each time it extracts, and sharun warns "Cannot find CA
  Certificates in host!" where there is no CA bundle.
- No update information (zsync) yet: that comes with the release job. x86_64
  only.
