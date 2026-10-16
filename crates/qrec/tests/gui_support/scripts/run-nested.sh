#!/usr/bin/env bash
# Run a Wayland client (`qrec --project <dir>`, say) inside a nested
# HEADLESS compositor and optionally grab a compositor-level PNG: the GUI
# spike's script, copied for manual runs (G§8.4). The tests use the Rust
# harness next to it (`gui_support/mod.rs`), which does the same with a
# TestEnv. Never touches the user's session: a private XDG_RUNTIME_DIR,
# DISPLAY/WAYLAND_DISPLAY unset for the compositor, GDK_BACKEND=wayland for
# the client (a failed connection can't fall back to the user's Xwayland).
# Tools come from PATH: run it inside the dev shell (sway, grim, wtype).
# Never /dev/dri/renderD128 (NVIDIA hangs on the dev machine): gles2 runs
# render on renderD129 with Mesa's EGL pinned.
#
#   COMP=sway|cage|weston  WLR=gles2|pixman|vulkan (wlroots renderer)
#   WESTON_RENDERER=gl|pixman|vulkan  OUT_SIZE=1600x1000  REFRESH=60
#   ISOLATE=1 (default): private XDG_CONFIG_HOME/XDG_DATA_HOME, GSETTINGS_BACKEND=memory
#            and a private D-Bus session WITHOUT service activation for the client
#            (no user gtk.css/settings.ini, no portal settings/accent color, no a11y bus)
#   GRIM_OUT=out.png GRIM_AFTER=4    compositor screenshot after N s
#   STRACE=1                         log every open() of compositor and client
#   run-nested.sh <client argv...>
set -uo pipefail
# Job control: every background job gets its own process group, so cleanup
# can kill a whole group (strace + its tracee, dbus-run-session + child, …).
set -m

here=$(cd "$(dirname "$0")/.." && pwd)
# Tools from PATH (the dev shell).
comp=${COMP:-sway}
wlr=${WLR:-gles2}
weston_renderer=${WESTON_RENDERER:-gl}
# Not SIZE: stdenv exports SIZE=size (the binutils tool name) in the dev env.
size=${OUT_SIZE:-1600x1000}
refresh=${REFRESH:-60}
grim_out=${GRIM_OUT:-}
grim_after=${GRIM_AFTER:-4}
client_timeout=${CLIENT_TIMEOUT:-90}
logdir=${LOGDIR:-$(cd "$here/../../../.." && pwd)/tmp/gui-shots/manual/logs}
tag=${TAG:-$comp-$(date +%H%M%S)}
mkdir -p "$logdir"

# GPU pinning (A§8.10): Mesa EGL only, Intel Vulkan ICD only, Intel VA.
export __EGL_VENDOR_LIBRARY_FILENAMES=/run/opengl-driver/share/glvnd/egl_vendor.d/50_mesa.json
export VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/intel_icd.x86_64.json
export VK_ICD_FILENAMES=$VK_DRIVER_FILES
export LIBVA_DRIVER_NAME=iHD
intel_node=/dev/dri/renderD129
# GPU_STRICT=1: also keep Vulkan's implicit layers off the NVIDIA node: Mesa's
# device-select layer opens every render node (renderD128 included), and the
# NVIDIA Optimus/present layers load libGLX_nvidia when their enable vars are set.
if [ "${GPU_STRICT:-0}" = 1 ]; then
  export NODEVICE_SELECT=1
  export VK_LOADER_LAYERS_DISABLE=VK_LAYER_NV_optimus,VK_LAYER_NV_present
  unset __NV_PRIME_RENDER_OFFLOAD __VK_LAYER_NV_optimus __GLX_VENDOR_LIBRARY_NAME DRI_PRIME
fi

# The runtime dir holds only sockets and is removed on exit; it's a fresh
# mktemp dir under $TMPDIR (not the repo's tmp/: socket paths must fit in
# sun_path's 108 bytes). Logs and screenshots go to the repo's tmp/.
rt=$(mktemp -d -t qrec-nested-rt.XXXXXX)
chmod 700 "$rt"
unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET SWAYSOCK I3SOCK NIRI_SOCKET
export XDG_RUNTIME_DIR=$rt

# Runs argv, exec'ing (so $! is the real process), optionally under strace.
st() {
  local name=$1
  shift
  if [ "${STRACE:-0}" = 1 ]; then
    exec strace -f -qq -e trace=open,openat,openat2 -o "$logdir/$tag-$name.strace" "$@"
  else
    exec "$@"
  fi
}

comp_pid=
client_pid=
bus_pid=
cleanup() {
  [ -n "$client_pid" ] && kill -TERM -- "-$client_pid" 2>/dev/null
  [ -n "$comp_pid" ] && kill -TERM -- "-$comp_pid" 2>/dev/null
  for _ in $(seq 50); do
    { [ -n "$comp_pid" ] && kill -0 -- "-$comp_pid" 2>/dev/null; } || break
    sleep 0.1
  done
  [ -n "$comp_pid" ] && kill -KILL -- "-$comp_pid" 2>/dev/null
  [ -n "$bus_pid" ] && kill -TERM -- "-$bus_pid" 2>/dev/null
  rm -rf "$rt"
}
trap cleanup EXIT

w=${size%x*}
h=${size#*x}

if [ "${ISOLATE:-1}" = 1 ]; then
  mkdir -p "$rt/config" "$rt/data"
  export XDG_CONFIG_HOME=$rt/config XDG_DATA_HOME=$rt/data GSETTINGS_BACKEND=memory
  # Nix keeps compiled schemas under share/gsettings-schemas/<pkg>/; GLib only
  # looks in <dir>/glib-2.0/schemas, so name them (libadwaita's fallback when
  # no Settings portal answers needs org.gnome.desktop.interface).
  schema_dirs=
  IFS=: read -r -a data_dirs <<<"${XDG_DATA_DIRS:-}"
  for d in "${data_dirs[@]}"; do
    for s in "$d"/gsettings-schemas/*/glib-2.0/schemas; do
      [ -f "$s/gschemas.compiled" ] && schema_dirs="$schema_dirs${schema_dirs:+:}$s"
    done
  done
  export GSETTINGS_SCHEMA_DIR=$schema_dirs
  # A private session bus with no service activation: no portals, no a11y
  # bus, no keyring get started on the client's behalf.
  cat >"$rt/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path=$rt/bus</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
  dbus-daemon --config-file="$rt/bus.conf" --nofork --nopidfile >"$logdir/$tag-bus.log" 2>&1 &
  bus_pid=$!
  export DBUS_SESSION_BUS_ADDRESS=unix:path=$rt/bus
fi

case $comp in
  sway)
    cfg="$rt/sway.cfg"
    cat >"$cfg" <<EOF
output HEADLESS-1 mode --custom ${w}x${h}@${refresh}Hz position 0 0 scale 1
default_border none
default_floating_border none
gaps inner 0
gaps outer 0
xwayland disable
swaybg_command -
swaynag_command -
focus_follows_mouse no
seat * hide_cursor 100
EOF
    (WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=$wlr \
      WLR_RENDER_DRM_DEVICE=$intel_node \
      st comp sway --unsupported-gpu -c "$cfg") >"$logdir/$tag-comp.log" 2>&1 &
    comp_pid=$!
    ;;
  cage) ;; # started below: cage runs the client itself
  weston)
    (st comp weston --backend=headless --renderer="$weston_renderer" \
      --width="$w" --height="$h" --refresh-rate=$((refresh * 1000)) --shell=kiosk \
      --socket=wayland-1 --idle-time=0 --no-config --debug) >"$logdir/$tag-comp.log" 2>&1 &
    comp_pid=$!
    ;;
  *) echo "unknown COMP $comp" >&2; exit 2 ;;
esac

export GDK_BACKEND=wayland
export GTK_A11Y=${GTK_A11Y:-none}

if [ "$comp" = cage ]; then
  (WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=$wlr \
    WLR_RENDER_DRM_DEVICE=$intel_node WLR_HEADLESS_OUTPUTS=1 \
    st comp cage -- timeout "$client_timeout" "$@") >"$logdir/$tag-comp.log" 2>&1 &
  comp_pid=$!
fi

sock=
for _ in $(seq 100); do
  sock=$(cd "$rt" && ls wayland-* 2>/dev/null | grep -v '\.lock$' | head -1)
  [ -n "$sock" ] && [ -S "$rt/$sock" ] && break
  sleep 0.1
done
if [ -z "$sock" ]; then
  echo "compositor socket never appeared; log: $logdir/$tag-comp.log" >&2
  tail -20 "$logdir/$tag-comp.log" >&2
  exit 3
fi
export WAYLAND_DISPLAY=$sock
echo "[nested] $comp up on $rt/$sock (log $logdir/$tag-comp.log)" >&2

if [ "$comp" != cage ]; then
  (st client timeout "$client_timeout" "$@") 2>"$logdir/$tag-client.log" &
  client_pid=$!
fi

if [ -n "$grim_out" ]; then
  sleep "$grim_after"
  if [ "$comp" = weston ]; then
    (cd "$(dirname "$grim_out")" && timeout 10 weston-screenshooter \
      >>"$logdir/$tag-shot.log" 2>&1) && echo "[nested] weston-screenshooter ok" >&2
    f=$(ls -t "$(dirname "$grim_out")"/wayland-screenshot-*.png 2>/dev/null | head -1)
    [ -n "$f" ] && mv "$f" "$grim_out" && echo "[nested] -> $grim_out" >&2
  else
    timeout 10 grim "$grim_out" >>"$logdir/$tag-shot.log" 2>&1 \
      && echo "[nested] grim ok -> $grim_out" >&2
  fi
fi

if [ "$comp" = cage ]; then
  wait "$comp_pid"; rc=$?   # cage exits with its client
  comp_pid=
else
  wait "$client_pid"; rc=$?
  client_pid=
fi
echo "[nested] client exit $rc (log $logdir/$tag-client.log)" >&2
exit $rc
