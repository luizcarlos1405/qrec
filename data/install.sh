#!/bin/sh
# Installs Qrec's desktop entry and icons under a prefix (docs/GUI.md G§6.4):
#
#   share/applications/com.rsensato.qrec.desktop
#   share/icons/hicolor/scalable/apps/com.rsensato.qrec.svg
#   share/icons/hicolor/{32x32,64x64,128x128}/apps/com.rsensato.qrec.png
#   share/icons/hicolor/symbolic/apps/com.rsensato.qrec-symbolic.svg
#
# The file and icon names are the app id (SPEC's App ID row; the window's
# Wayland app_id, so the compositor finds the entry).
#
# The app icon is a PLACEHOLDER until the pen export (G§0.6): the theme's
# own placeholder art, crates/qrec-gui/resources/art/app-icon-24.svg, with
# its {{token}} colors taken from Swamp Night's tokens (the reference
# theme), and PNGs rendered from it with rsvg-convert. The symbolic icon is
# the REC glyph, the app icon's symbolic form (DESIGN.md), also a
# placeholder.
#
# Needs rsvg-convert (librsvg), sed and install. The Nix package runs it in
# postInstall (flake.nix); `data/install.sh ~/.local` installs it for one
# person.
#
# Usage: data/install.sh <prefix>
set -eu

prefix=${1:?usage: data/install.sh <prefix>}
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
id=com.rsensato.qrec
tokens=$root/crates/qrec-gui/resources/theme/tokens/swamp-night.css
art=$root/crates/qrec-gui/resources/art/app-icon-24.svg
glyph=$root/crates/qrec-gui/resources/icons/scalable/actions/qrec-record-symbolic.svg
icons=$prefix/share/icons/hicolor

for file in "$here/$id.desktop" "$tokens" "$art" "$glyph"; do
  [ -f "$file" ] || { echo "install.sh: missing $file" >&2; exit 1; }
done
command -v rsvg-convert >/dev/null || {
  echo "install.sh: rsvg-convert (librsvg) isn't on PATH" >&2
  exit 1
}

install -d "$prefix/share/applications" "$icons/scalable/apps" "$icons/symbolic/apps"
install -m 644 "$here/$id.desktop" "$prefix/share/applications/$id.desktop"

# Each {{name}} in the art takes the token --qrec-<name>'s color.
script=
for name in $(grep -o '{{[a-z0-9-]*}}' "$art" | tr -d '{}' | sort -u); do
  color=$(sed -n "s/^ *--qrec-$name: *\(#[0-9A-Fa-f]\{6\}\);.*/\1/p" "$tokens")
  [ -n "$color" ] || { echo "install.sh: no color --qrec-$name in $tokens" >&2; exit 1; }
  script="${script}s/{{$name}}/$color/g;"
done
svg=$icons/scalable/apps/$id.svg
sed -e "$script" "$art" >"$svg"
chmod 644 "$svg"
if grep -q '{{' "$svg"; then
  echo "install.sh: $svg still has a {{token}}" >&2
  exit 1
fi

for size in 32 64 128; do
  dir=$icons/${size}x${size}/apps
  install -d "$dir"
  rsvg-convert --width "$size" --height "$size" --format png --output "$dir/$id.png" "$svg"
  chmod 644 "$dir/$id.png"
done

install -m 644 "$glyph" "$icons/symbolic/apps/$id-symbolic.svg"
