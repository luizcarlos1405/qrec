# Third-party notices

Qrec's own code is under the MIT license (`LICENSE`). These parts come from
others, under their own licenses, and ship with every package:

| What | Where | License |
| --- | --- | --- |
| Lucide icons (the GUI's symbolic icons) | `crates/qrec-gui/resources/icons/` | ISC, `crates/qrec-gui/resources/icons/LICENSE-lucide.txt`; installed as `share/licenses/qrec/LICENSE-lucide.txt` |
| Fraunces, Atkinson Hyperlegible Next and JetBrains Mono (the GUI's fonts) | installed in `share/qrec/fonts/` | SIL Open Font License 1.1; their texts are installed in `share/licenses/qrec-fonts/` |

Qrec links GTK 4, libadwaita, GStreamer, GES, GLib and their dependencies,
which are under the LGPL-2.1-or-later or similar licenses, and it runs
`ffmpeg` and `ffprobe` as separate programs. The Nix flake, the AUR package
and the .deb take these from the distribution. The AppImage bundles them,
together with ffmpeg, x264 and GStreamer's x264enc, which are under the
GPL-2.0-or-later; its README (`packaging/appimage/README.md`) lists what it
bundles, at which versions, and where their sources are.
