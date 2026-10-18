#!/bin/sh
# Installs Qrec's desktop entry and icons under a prefix (docs/GUI.md G§6.4):
#
#   share/applications/com.rsensato.qrec.desktop
#   share/icons/hicolor/scalable/apps/com.rsensato.qrec.svg
#   share/icons/hicolor/{16x16,32x32,48x48,64x64,128x128,256x256,512x512}/apps/com.rsensato.qrec.png
#   share/icons/hicolor/symbolic/apps/com.rsensato.qrec-symbolic.svg
#   share/licenses/qrec/LICENSE-lucide.txt
#
# The file and icon names are the app id (SPEC's App ID row; the window's
# Wayland app_id, so the compositor finds the entry).
#
# The app icon is the pen file's (`ptPs6`, design/qrec.pen): the scalable
# SVG and the hicolor PNGs in data/icons/ are written by
# crates/qrec-gui/tests/art.rs from the 2026-10-17 export (the PNGs are the
# pen's own renders: rsvg's differ visibly at the small sizes). The symbolic
# icon is the REC glyph, the app icon's symbolic form (DESIGN.md). The
# GUI's symbolic icons are Lucide's: their license goes with the package.
#
# Needs only sh and install. The Nix package runs it in postInstall
# (flake.nix); `data/install.sh ~/.local` installs it for one person.
#
# Usage: data/install.sh <prefix>
set -eu

prefix=${1:?usage: data/install.sh <prefix>}
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
id=com.rsensato.qrec
art=$here/icons/hicolor
glyph=$root/crates/qrec-gui/resources/icons/scalable/actions/qrec-record-symbolic.svg
license=$root/crates/qrec-gui/resources/icons/LICENSE-lucide.txt
sizes="16 32 48 64 128 256 512"
icons=$prefix/share/icons/hicolor

for file in "$here/$id.desktop" "$art/scalable/apps/$id.svg" "$glyph" "$license"; do
  [ -f "$file" ] || { echo "install.sh: missing $file" >&2; exit 1; }
done
for size in $sizes; do
  png=$art/${size}x${size}/apps/$id.png
  [ -f "$png" ] || { echo "install.sh: missing $png" >&2; exit 1; }
done

install -D -m 644 "$here/$id.desktop" "$prefix/share/applications/$id.desktop"
install -D -m 644 "$art/scalable/apps/$id.svg" "$icons/scalable/apps/$id.svg"
for size in $sizes; do
  install -D -m 644 "$art/${size}x${size}/apps/$id.png" "$icons/${size}x${size}/apps/$id.png"
done
install -D -m 644 "$glyph" "$icons/symbolic/apps/$id-symbolic.svg"
install -D -m 644 "$license" "$prefix/share/licenses/qrec/LICENSE-lucide.txt"
