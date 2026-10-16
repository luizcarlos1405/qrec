#!/usr/bin/env bash
# Which GPU device nodes and GPU driver libraries a traced run opened
# (G§3.13, G§8.6): only the chosen render node may show, never NVIDIA's.
# Copied from the GUI spike (G§8.4).
#
# Usage: gpu-opens.sh <file.strace>...
#   strace -f -qq -e trace=open,openat,openat2 -o run.strace qrec --project …
#   (run-nested.sh with STRACE=1 writes <tag>-comp.strace and <tag>-client.strace)
set -u
status=0
# NVIDIA's render nodes by PCI vendor (0x10de): their numbers swap between
# boots on the dev machine, so never a fixed renderD128.
nvidia_nodes=
for vendor in /sys/class/drm/renderD*/device/vendor; do
  [ -r "$vendor" ] || continue
  if [ "$(cat "$vendor")" = 0x10de ]; then
    node=$(basename "$(dirname "$(dirname "$vendor")")")
    nvidia_nodes="$nvidia_nodes|$node"
  fi
done
nvidia_pattern="/dev/nvidia|nvidia$nvidia_nodes"
for f in "$@"; do
  [ -f "$f" ] || { echo "no such file: $f" >&2; status=2; continue; }
  echo "== $(basename "$f")"
  grep -E '/dev/dri|/dev/nvidia|libEGL_|libGLX_|libnvidia|libcuda|icd\.d|vulkan_intel|libvulkan_|iris_dri|libgallium|dri_gbm|egl_vendor' "$f" \
    | grep -v ENOENT \
    | sed -E 's/^[0-9]+ +//; s/, O_[A-Z_|]+.*//; s/openat\(AT_FDCWD, //' \
    | sort | uniq -c | sort -rn | head -30
  echo "-- NVIDIA touches:"
  if grep -E "$nvidia_pattern" "$f" | grep -v ENOENT | sed -E 's/^[0-9]+ +//' | sort | uniq -c | head -10 | grep .; then
    status=1
  fi
done
exit $status
