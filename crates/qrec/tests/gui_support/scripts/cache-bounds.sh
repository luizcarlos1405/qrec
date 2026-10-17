#!/usr/bin/env bash
# G§8.6's cache bounds: qrec-gui's nested texture test
# (`media::thumbs`' ignored `textures_in_a_nested_compositor`: real strip
# and frame textures drawn by GSK, then `ThumbCache::bytes()` and
# `PeakCache::bytes()` while a one-hour fixture scrolls) as a client of a
# private headless sway on pixman (`run-nested.sh`: never the user's
# session, nothing renders on a GPU: GDK_DISABLE=gl,vulkan), and its
# measurement line ("an hour scrolled: at most … thumbnail bytes; peaks …").
#
# Run it inside the dev shell (sway, ffmpeg), from anywhere:
#   crates/qrec/tests/gui_support/scripts/cache-bounds.sh [<qrec_gui test binary>]
# Without an argument it finds the binary cargo built
# (`cargo test -p qrec-gui --lib --no-run`). Logs go to
# tmp/gui-shots/cache-bounds/ (the test's output: cache-bounds-client.log).
# Exit status: the test's (0 passed), 2 without a binary.
set -u
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../../.." && pwd)
bin=${1:-}
if [ -z "$bin" ]; then
  bin=$(cd "$repo" && cargo test -p qrec-gui --lib --no-run --message-format=json 2>/dev/null \
    | sed -n 's/.*"executable":"\([^"]*qrec_gui-[^"]*\)".*/\1/p' | tail -1)
fi
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  echo "cache-bounds.sh: no qrec-gui test binary (${bin:-none})" >&2
  exit 2
fi
logdir=$repo/tmp/gui-shots/cache-bounds
mkdir -p "$logdir"
WLR=pixman GDK_DISABLE=gl,vulkan CLIENT_TIMEOUT=${CLIENT_TIMEOUT:-900} LOGDIR=$logdir TAG=cache-bounds \
  "$here/run-nested.sh" "$bin" --ignored --test-threads=1 --nocapture textures_in_a_nested_compositor \
  >"$logdir/cache-bounds-run.log" 2>&1
status=$?
grep -h -E 'an hour scrolled|test result|panicked' "$logdir/cache-bounds-run.log" "$logdir/cache-bounds-client.log"
exit $status
