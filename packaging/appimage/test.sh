#!/bin/sh
# Tries the AppImage on other distributions, in docker containers
# (packaging/appimage/README.md, "Try it").
#
#   packaging/appimage/test.sh [APPIMAGE] [IMAGE...]
#   QREC_TEST_VA=1 packaging/appimage/test.sh ...   also this machine's Intel or AMD render node
#
# APPIMAGE defaults to the newest one in $QREC_APPIMAGE_DIR/out; the images
# to ubuntu:24.04 debian:13 fedora:latest archlinux:latest. Each container
# gets the AppImage read-only and no /dev/fuse, so uruntime extracts it; it
# installs only Xvfb and D-Bus (no GTK, GStreamer or ffmpeg of its own) and
# runs, with the fake capture backend:
#
#   1. qrec --version, qrec doctor (text and JSON; with the real backend, so
#      its VA-API probes run), which must name the AppImage and give no
#      package manager's hint (the AppImage bundles what it would install),
#      qrec sources list;
#   2. qrec init, a scene (a screen, a camera, a mic), a take (record start,
#      record stop) and an MP4 export, checked with the bundled ffprobe;
#   3. a take started by one AppImage run and stopped by another, by a host
#      of its own that must outlive the CLI that started it (PIP-05);
#   4. the window on a private Xvfb: a screenshot, its renderer (Cairo
#      without a render node, GL with QREC_TEST_VA's), the GL context GTK
#      made and its log's criticals;
#   5. --appimage-extract-and-run.
#
# On a host whose kernel hands AppImages to an interpreter (NixOS's
# programs.appimage.binfmt, which runs appimage-run), the containers get a
# copy without the AppImage magic, and the AppImage itself runs here too:
# qrec --version and qrec doctor, through that interpreter.
#
# Results go to $QREC_APPIMAGE_DIR/test/<image>/ (summary.txt first) and the
# screenshots to $QREC_APPIMAGE_DIR/shots/<image>.png. No host display,
# session bus or FUSE is used.
set -eu

DEFAULT_IMAGES="ubuntu:24.04 debian:13 fedora:latest archlinux:latest"

# --- In the container --------------------------------------------------------

A=/opt/qrec.AppImage
OUT=/out

say() { printf '%s\n' "$*" >> "$OUT/summary.txt"; }
# step NAME COMMAND...: runs with a deadline, output to $OUT/NAME.txt.
step() {
	name=$1
	shift
	if timeout 180 "$@" > "$OUT/$name.txt" 2>&1; then
		say "ok    $name"
	else
		say "FAIL  $name (exit $?), see $name.txt"
	fi
}

install_tools() {
	if command -v apt-get >/dev/null; then
		apt-get update -qq
		DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
			xvfb dbus procps ca-certificates >/dev/null
	elif command -v dnf >/dev/null; then
		dnf -y -q install xorg-x11-server-Xvfb dbus-daemon dbus-tools procps-ng >/dev/null
	elif command -v pacman >/dev/null; then
		pacman -Sy --noconfirm --needed xorg-server-xvfb dbus procps-ng >/dev/null
	fi
}

host_facts() {
	. /etc/os-release
	say "host  $PRETTY_NAME, kernel $(uname -r)"
	say "host  glibc: $(ldd --version 2>&1 | head -n 1)"
	gtk=$(find / -xdev \( -name 'libgtk-4.so*' -o -name 'libgstreamer-1.0.so*' -o -name 'libadwaita-1.so*' \) \
		-not -path '/tmp/*' 2>/dev/null | head -n 3)
	say "host  GTK/GStreamer/libadwaita installed: ${gtk:-none}"
	say "host  /dev/fuse: $([ -e /dev/fuse ] && echo present || echo absent)"
	say "host  FUSE tools: $(command -v fusermount3 fusermount 2>/dev/null | tr '\n' ' ')"
}

cli() {
	step version "$A" --version
	# The doctor with the real backend, as people run it: without a display
	# screens are off, everything else must come from the bundle. (The fake
	# backend skips the VA-API probes.)
	step doctor env -u QREC_CAPTURE_BACKEND "$A" doctor
	env -u QREC_CAPTURE_BACKEND "$A" doctor --json > "$OUT/doctor.json" 2>"$OUT/doctor-json.stderr" || :
	# It knows the AppImage it runs from (APPIMAGE, APPDIR) and names no
	# package manager: the AppImage bundles what one would install.
	if grep -q "^AppImage: $A (APPDIR=/" "$OUT/doctor.txt" \
		&& grep -q "\"appImage\":{\"dir\":\"/[^\"]*\",\"path\":\"$A\"}" "$OUT/doctor.json"; then
		say "ok    doctor-appimage: $(grep '^AppImage: ' "$OUT/doctor.txt")"
	else
		say "FAIL  doctor-appimage: no AppImage line in doctor.txt, or none in doctor.json"
	fi
	managers=$(grep -n -w -E 'apt|apt-get|dnf|pacman|zypper|sudo' "$OUT/doctor.txt" || :)
	if [ -z "$managers" ]; then
		say "ok    doctor-hints: no package manager named"
	else
		say "FAIL  doctor-hints: $managers"
	fi
	step sources "$A" sources list
}

take_and_export() {
	mkdir -p /work/demo && cd /work/demo
	step init "$A" init
	step scene "sh" -c "'$A' scene layer add screen --output FAKE-1 &&
		'$A' scene layer add camera --device /dev/fake0 &&
		'$A' scene audio add mic && '$A' status"
	step record-start "$A" record start --json
	sleep 6
	step record-stop "$A" record stop --json
	step export "$A" export --json
	mp4=$(find /work/demo/exports -name '*.mp4' | head -n 1)
	if [ -n "$mp4" ]; then
		step ffprobe "$A" ffprobe -v error -show_entries \
			stream=codec_type,codec_name,profile,width,height,r_frame_rate:format=duration,size \
			-of default=noprint_wrappers=1 "$mp4"
	else
		say "FAIL  ffprobe: no MP4 in exports/"
	fi
}

# PIP-05: the CLI that starts a take exits; its host must keep recording.
outliving_host() {
	cd /work/demo
	# A host of its own: the first take's must be gone (it exits once idle).
	i=0
	while pgrep -f 'host --headless' >/dev/null && [ "$i" -lt 60 ]; do
		sleep 1
		i=$((i + 1))
	done
	pgrep -f 'host --headless' >/dev/null && say "info  the first take's host is still running after 60 s"
	step pip05-start "$A" record start --json
	sleep 8
	ps -eo pid,ppid,sid,stat,etimes,args > "$OUT/pip05-ps.txt" 2>&1
	if grep -q '[h]ost --headless' "$OUT/pip05-ps.txt"; then
		say "ok    pip05-host-alive (8 s after its CLI exited): $(grep '[h]ost --headless' "$OUT/pip05-ps.txt" | awk '{print "pid " $1 ", parent " $2 ", " $5 " s old"}')"
	else
		say "FAIL  pip05-host-alive: no headless host 8 s after the CLI exited"
	fi
	step pip05-stop "$A" record stop --json
	cat /work/demo/.qrec/logs/*host.log > "$OUT/pip05-host-logs.txt" 2>/dev/null || :
}

window() {
	Xvfb :77 -screen 0 1600x1000x24 -nolisten tcp -ac > "$OUT/xvfb.log" 2>&1 &
	xvfb=$!
	sleep 2
	cd /work/demo
	# GDK_DEBUG=opengl: GTK logs the GL context it renders with;
	# GSK_DEBUG=renderer: the renderer it picked.
	DISPLAY=:77 GDK_DEBUG=opengl GSK_DEBUG=renderer dbus-run-session -- "$A" --project /work/demo \
		> "$OUT/gui.log" 2>&1 &
	gui=$!
	sleep 30
	if kill -0 "$gui" 2>/dev/null; then
		say "ok    window running after 30 s"
	else
		say "FAIL  window exited early, see gui.log"
	fi
	DISPLAY=:77 timeout 60 "$A" ffmpeg -loglevel error -f x11grab -video_size 1600x1000 \
		-i :77 -frames:v 1 -y "$OUT/shot.png" > "$OUT/shot.txt" 2>&1 \
		&& say "ok    screenshot" || say "FAIL  screenshot, see shot.txt"
	kill "$gui" 2>/dev/null || :
	sleep 3
	kill -9 "$gui" 2>/dev/null || :
	kill "$xvfb" 2>/dev/null || :
	gl=$(grep -i -E '^ - vendor|^ - device|opengl (es )?version|using opengl backend' "$OUT/gui.log" \
		| sort -u | tr '\n' ' ')
	say "info  GL: ${gl:-no GL context logged}"
	# Qrec's own log (its state folder): what it set up for the GPU.
	cat "$HOME"/.local/state/qrec/gui-*.log > "$OUT/gui-qrec.log" 2>/dev/null || :
	grep 'gpu: ' "$OUT/gui-qrec.log" | sed 's/^.*gpu: /info  gpu: /' >> "$OUT/summary.txt" || :
	renderer=$(grep -o -E "Using renderer '[A-Za-z]+'" "$OUT/gui.log" | sort -u | tr '\n' ' ')
	say "info  renderer: ${renderer:-not logged}"
	# Without a render node the window draws with Cairo, which Qrec chose
	# (the AppImage's Mesa has no llvmpipe), and no GL context is made; with
	# one (QREC_TEST_VA=1), GL.
	if ls /dev/dri/renderD* >/dev/null 2>&1; then
		if [ -n "$gl" ] && ! grep -q 'draws with Cairo' "$OUT/gui-qrec.log"; then
			say "ok    window-renderer: GL, with a render node"
		else
			say "FAIL  window-renderer: no GL context with a render node, see gui.log and gui-qrec.log"
		fi
	elif grep -q 'the window draws with Cairo (GDK_DISABLE=gl,vulkan)' "$OUT/gui-qrec.log" \
		&& [ -z "$gl" ] && grep -q "Using renderer 'GskCairoRenderer'" "$OUT/gui.log"; then
		say "ok    window-renderer: Cairo, without a render node"
	else
		say "FAIL  window-renderer: not Cairo without a render node, see gui.log and gui-qrec.log"
	fi
	criticals=$(grep -c -E 'CRITICAL|Gtk-WARNING|ERROR' "$OUT/gui.log" || :)
	say "info  gui.log: $criticals CRITICAL/Gtk-WARNING/ERROR lines"
	grep -E 'CRITICAL|Gtk-WARNING|ERROR' "$OUT/gui.log" | head -n 20 >> "$OUT/summary.txt" || :
}

extract_and_run() {
	step extract-and-run "$A" --appimage-extract-and-run --version
	step extract-and-run-doctor "$A" --appimage-extract-and-run doctor
}

in_container() {
	unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET DBUS_SESSION_BUS_ADDRESS
	trap 'chown -R "$HOST_UID:$HOST_GID" "$OUT"' EXIT
	: > "$OUT/summary.txt"
	install_tools
	export HOME=/root QREC_CAPTURE_BACKEND=fake XDG_RUNTIME_DIR=/tmp/runtime
	mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
	host_facts
	cli
	take_and_export
	outliving_host
	window
	extract_and_run
}

# --- On the host -------------------------------------------------------------

host() {
	here=$(cd "$(dirname "$0")" && pwd)
	repo=$(cd "$here/../.." && pwd)
	work=${QREC_APPIMAGE_DIR:-$repo/tmp/appimage}
	appimage=${1:-}
	if [ -z "$appimage" ] || [ ! -f "$appimage" ]; then
		appimage=$(ls -t "$work"/out/*.AppImage 2>/dev/null | head -n 1)
	else
		shift
	fi
	[ -n "$appimage" ] || { echo "test.sh: no AppImage in $work/out" >&2; exit 1; }
	appimage=$(cd "$(dirname "$appimage")" && pwd)/$(basename "$appimage")
	original=$appimage
	binfmt=
	mkdir -p "$work/test"
	# A kernel that hands AppImages to an interpreter of the host's
	# (binfmt_misc; NixOS's programs.appimage.binfmt runs appimage-run) can't
	# start one inside a container, where that interpreter doesn't exist. The
	# containers then get a copy without the AppImage magic (ELF ident bytes
	# 8-10, "AI\2"), which the kernel runs itself. uruntime doesn't read those
	# bytes: the copy runs as the AppImage does on a host without the handler.
	if grep -qs '^enabled' /proc/sys/fs/binfmt_misc/appimage_type_2; then
		copy=$work/test/$(basename "$appimage" .AppImage)-no-binfmt.AppImage
		cp "$appimage" "$copy"
		printf '\000\000\000' | dd of="$copy" bs=1 seek=8 conv=notrunc status=none
		echo "test.sh: this kernel runs AppImages through binfmt_misc; testing $copy" >&2
		appimage=$copy
		binfmt=$(sed -n 's/^interpreter //p' /proc/sys/fs/binfmt_misc/appimage_type_2)
	fi
	# shellcheck disable=SC2086
	[ $# -gt 0 ] || set -- $DEFAULT_IMAGES
	# QREC_TEST_VA=1 also gives each container this machine's first Intel or
	# AMD render node (never an NVIDIA one), so VA-API runs on the bundled
	# drivers. Containers see no other device.
	devices=
	suffix=
	if [ "${QREC_TEST_VA:-0}" = 1 ]; then
		for node in /sys/class/drm/renderD*; do
			case "$(cat "$node/device/vendor" 2>/dev/null)" in
				0x8086 | 0x1002)
					devices="--device /dev/dri/${node##*/}"
					suffix=-va
					break
					;;
			esac
		done
		[ -n "$devices" ] || { echo "test.sh: no Intel or AMD render node" >&2; exit 1; }
	fi
	mkdir -p "$work/shots"
	failed=
	for image do
		name=$(printf '%s' "$image" | tr ':/' '--')$suffix
		out=$work/test/$name
		rm -rf "$out" && mkdir -p "$out"
		echo "== $image ($appimage) $devices"
		# shellcheck disable=SC2086
		docker run --rm --name "qrec-appimage-test-$name" --cpus=3 $devices \
			-v "$appimage:/opt/qrec.AppImage:ro" -v "$here/test.sh:/opt/test.sh:ro" \
			-v "$out:/out" -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
			"$image" nice -n 19 sh /opt/test.sh --in-container || :
		[ ! -f "$out/shot.png" ] || cp "$out/shot.png" "$work/shots/$name.png"
		if [ -s "$out/summary.txt" ]; then
			cat "$out/summary.txt"
			! grep -q '^FAIL' "$out/summary.txt" || failed="$failed $name"
		else
			echo "FAIL  $name wrote no summary"
			failed="$failed $name"
		fi
	done
	# Where the kernel hands AppImages to an interpreter, the AppImage as
	# people start it there, on this host: NixOS's appimage-run unpacks it
	# (squashfs only) into ~/.cache/appimage-run/<sha256> and runs it in an
	# FHS sandbox. That copy goes again after, unless it was there before.
	if [ -n "$binfmt" ]; then
		out=$work/test/host-binfmt
		rm -rf "$out" && mkdir -p "$out"
		cache=${XDG_CACHE_HOME:-$HOME/.cache}/appimage-run/$(sha256sum "$original" | cut -d' ' -f1)
		had=
		[ ! -e "$cache" ] || had=1
		echo "== this host, through $binfmt ($original)"
		host_step() {
			name=$1
			shift
			if timeout 600 "$original" "$@" > "$out/$name.txt" 2>&1; then
				echo "ok    $name" >> "$out/summary.txt"
			else
				echo "FAIL  $name (exit $?), see $name.txt" >> "$out/summary.txt"
			fi
		}
		host_step version --version
		host_step doctor doctor
		[ -n "$had" ] || rm -rf "$cache"
		cat "$out/summary.txt"
		! grep -q '^FAIL' "$out/summary.txt" || failed="$failed host-binfmt"
	fi
	# A failed check fails the run (and CI).
	[ -z "$failed" ] || { echo "test.sh: checks failed on:$failed" >&2; exit 1; }
}

if [ "${1:-}" = --in-container ]; then
	in_container
else
	host "$@"
fi
