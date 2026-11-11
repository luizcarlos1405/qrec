#!/bin/sh
# Builds Qrec's AppImage in an Arch Linux container (packaging/appimage/README.md).
#
#   packaging/appimage/build.sh             the AppImage, in $QREC_APPIMAGE_DIR/out
#   packaging/appimage/build.sh --builder   (re)makes the builder image only
#
# The host needs docker and nothing else. The first run makes the builder
# image: Arch Linux with the packages of one day of the Arch Linux Archive,
# plus pkgforge-dev's Mesa without LLVM and glycin-ng. Each build then runs a
# container from it that compiles the release `qrec`, installs it to /usr
# with its fonts, desktop entry and icons, and bundles it with quick-sharun
# (pkgforge-dev's Anylinux AppImages): glibc, GTK, libadwaita, GStreamer,
# ffmpeg and Mesa come from the AppImage, none from the host.
#
# Containers run at nice 19 on at most $JOBS CPUs (default 3), and write only
# under $QREC_APPIMAGE_DIR (default: <repo>/tmp/appimage), owned by you.
set -eu

# --- Pins: each changes what the AppImage holds (the README lists them) ------

# archlinux:latest of 2026-11-09.
ARCH_IMAGE=docker.io/library/archlinux@sha256:d4594bb2300cba11f41c2f27923eb56f63d9988909c402f9c95517d1e362c899
# The Arch Linux Archive day every Arch package comes from.
ARCH_SNAPSHOT=${ARCH_SNAPSHOT:-2026/11/09}
# pkgforge-dev/Anylinux-AppImages at 2026-11-09: quick-sharun.sh, which pins
# sharun 3.8.3, appimagetool 0.5.6 (uruntime, DwarFS) and cross-libc-dlopen
# 0.2.10 by sha256.
ANYLINUX_COMMIT=3bc6529073bf952de8db7b62c86380dbcdb38b64
QUICK_SHARUN_SHA256=0329d9d6742d741a8cecf1c09d825016c8c5d911b316c3bdabf0cca993fd5b10
DEBLOATED_URL=https://github.com/pkgforge-dev/archlinux-pkgs-debloated/releases/download/continuous
# Bump when the builder's package list changes.
BUILDER_REVISION=${BUILDER_REVISION:-2}

# The GUI's fonts (crates/qrec-gui/src/theme/fonts.rs), from the releases
# nixpkgs packages; the hashes are the font files'. Fraunces' and Atkinson
# Hyperlegible Next's are the Nix package's bytes; JetBrains Mono's is the
# upstream release build (nixpkgs builds 2.304 from its sources).
FRAUNCES_URL=https://github.com/undercasetype/Fraunces/releases/download/1.000/UnderCaseType_Fraunces_1.000.zip
FRAUNCES_TTF_SHA256=7464bf3306f218e5e494ad830b18ecdcfb405868addcd7f7128d0d024f8589d1
ATKINSON_URL=https://github.com/googlefonts/atkinson-hyperlegible-next/archive/7925f50f649b3813257faf2f4c0b381011f434f1.tar.gz
ATKINSON_TTF_SHA256=5a455d1cfa099b601ab70751bb9673e8fe1854dc4500c80e1a220d0d75e31745
JETBRAINS_URL=https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip
JETBRAINS_TTF_SHA256=662a196d58f1183bf2d77428b6d5283fe3f45161ab021bea4036bc98e5cac016

# What the builder installs. qsv, msdk and hip are separate Arch packages and
# stay out; nvcodec is in gst-plugins-bad-libs and is deleted before bundling.
PACKAGES="
	base-devel binutils curl patchelf pkgconf rust unzip
	dbus xorg-server-xvfb xorg-xauth
	glib2 gtk4 libadwaita
	adwaita-icon-theme hicolor-icon-theme gsettings-desktop-schemas
	gdk-pixbuf2 librsvg xkeyboard-config
	gstreamer gst-plugins-base gst-plugins-base-libs gst-plugins-good
	gst-plugins-bad gst-plugins-bad-libs gst-plugins-ugly gst-libav
	gst-editing-services gst-plugin-gtk4 gst-plugin-va gst-plugin-pipewire
	pipewire libpulse
	ffmpeg x264 openh264 libvpx opus svt-av1
	libva intel-media-driver libva-intel-driver
	wl-clipboard
"
# From pkgforge-dev/archlinux-pkgs-debloated: Mesa and its Vulkan drivers
# without LLVM (quick-sharun refuses an LLVM-linked libgallium, and their
# radeonsi drops the newer-kernel check), and glycin-ng, whose image loaders
# need no bwrap (no user namespaces).
DEBLOATED="mesa-mini vulkan-intel-mini vulkan-radeon-mini vulkan-virtio-mini glycin-mini"
# GStreamer plugins Qrec must not load. nvcodec, qsv, msdk and hip probe
# every GPU during the registry scan, which can wedge on hybrid NVIDIA
# machines (flake.nix). gst-python, which Arch's GES pulls in, would start a
# Python without its standard library in every scan.
DROP_GST_PLUGINS="libgstnvcodec.so libgstqsv.so libgstmsdk.so libgsthip.so libgstpython.so"

JOBS=${JOBS:-3}
PREFIX_URL=https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/$ANYLINUX_COMMIT/useful-tools

log() { printf '\033[1;34m==> %s\033[0m\n' "$*" >&2; }
die() { printf '\033[1;31mbuild.sh: %s\033[0m\n' "$*" >&2; exit 1; }

# fetch URL FILE SHA256: download once into the cache, check the hash.
fetch() {
	if [ ! -f "$2" ]; then
		curl -fL --retry 5 --retry-connrefused -o "$2.part" "$1"
		mv "$2.part" "$2"
	fi
	if [ -n "$3" ] && ! printf '%s  %s\n' "$3" "$2" | sha256sum -c --quiet -; then
		rm -f "$2"
		die "sha256 mismatch for $1"
	fi
}

# check FILE SHA256
check() {
	printf '%s  %s\n' "$2" "$1" | sha256sum -c --quiet - || die "sha256 mismatch for $1"
}

# --- In the container: the builder image ----------------------------------

prepare_builder() {
	printf 'Server = https://archive.archlinux.org/repos/%s/$repo/os/$arch\n' \
		"$ARCH_SNAPSHOT" > /etc/pacman.d/mirrorlist
	pacman-key --init >/dev/null 2>&1
	pacman-key --populate archlinux >/dev/null 2>&1
	# -uu: the snapshot may be older than the image's packages.
	# shellcheck disable=SC2086
	pacman -Syyuu --noconfirm --needed $PACKAGES
	mkdir -p /opt/qrec-appimage /tmp/debloated
	# What get-debloated-pkgs.sh --add-mesa glycin-mini installs, fetched
	# here so their hashes can be kept: upstream publishes them only as a
	# rolling "continuous" release, the one input that can't be pinned.
	mesa_version=$(pacman -Q mesa | cut -d' ' -f2)
	for pkg in $DEBLOATED; do
		curl -fL --retry 5 --retry-connrefused -o "/tmp/debloated/$pkg-x86_64.pkg.tar.zst" \
			"$DEBLOATED_URL/$pkg-x86_64.pkg.tar.zst"
	done
	# Must come after every `pacman -S`, which would put Arch's back. --ask 4
	# lets glycin-ng replace glycin.
	pacman -U --noconfirm --ask 4 /tmp/debloated/*.pkg.tar.zst
	# Built for the Arch of their day: the Mesa must be the snapshot's.
	[ "$(pacman -Q mesa | cut -d' ' -f2)" = "$mesa_version" ] \
		|| die "the debloated Mesa is $(pacman -Q mesa), the snapshot's is $mesa_version"
	(cd /tmp/debloated && sha256sum ./*.pkg.tar.zst) | sed 's| \./| |' > /opt/qrec-appimage/debloated.txt
	for pkg in mesa vulkan-intel vulkan-radeon vulkan-virtio glycin-ng; do
		pacman -Qi "$pkg" | awk -v p="$pkg" -F' *: ' \
			'/^Version/{v=$2} /^Build Date/{d=$2} END{print "  " p " " v ", built " d}'
	done >> /opt/qrec-appimage/debloated.txt
	cat /opt/qrec-appimage/debloated.txt
	pacman -Scc --noconfirm >/dev/null
	rm -rf /var/cache/pacman/pkg/* /tmp/*
	pacman -Q > /opt/qrec-appimage/packages.txt
	printf '%s\n' "$ARCH_SNAPSHOT" > /opt/qrec-appimage/snapshot
}

# --- In the container: build qrec and bundle it ---------------------------

build_qrec() {
	log "Building qrec $QREC_VERSION (release, $JOBS jobs)"
	export CARGO_HOME=/work/cargo-home CARGO_TARGET_DIR=/work/target
	export CARGO_BUILD_JOBS="$JOBS" CARGO_TERM_COLOR=never
	(cd /src && nice -n 19 cargo build --release --locked -p qrec)
	install -Dm755 /work/target/release/qrec /usr/bin/qrec
	# The desktop entry, the hicolor icons and Lucide's license.
	sh /src/data/install.sh /usr
}

install_fonts() {
	log "Fonts"
	cache=/work/cache/fonts
	mkdir -p "$cache/x"
	fetch "$FRAUNCES_URL" "$cache/fraunces.zip" ""
	fetch "$ATKINSON_URL" "$cache/atkinson.tar.gz" ""
	fetch "$JETBRAINS_URL" "$cache/jetbrains-mono.zip" ""
	unzip -o -q -j "$cache/fraunces.zip" \
		'UnderCaseType_Fraunces_1.000/Fonts - Desktop/Fraunces\[SOFT,WONK,opsz,wght\].ttf' -d "$cache/x"
	tar -xzf "$cache/atkinson.tar.gz" -C "$cache/x" --strip-components=3 \
		'atkinson-hyperlegible-next-7925f50f649b3813257faf2f4c0b381011f434f1/fonts/variable/AtkinsonHyperlegibleNext[wght].ttf'
	unzip -o -q -j "$cache/jetbrains-mono.zip" 'fonts/variable/JetBrainsMono\[wght\].ttf' -d "$cache/x"
	check "$cache/x/Fraunces[SOFT,WONK,opsz,wght].ttf" "$FRAUNCES_TTF_SHA256"
	check "$cache/x/AtkinsonHyperlegibleNext[wght].ttf" "$ATKINSON_TTF_SHA256"
	check "$cache/x/JetBrainsMono[wght].ttf" "$JETBRAINS_TTF_SHA256"
	# Where the GUI looks: <exe>/../share/qrec/fonts. quick-sharun copies
	# /usr/share/qrec, the datadir named after the binary.
	install -Dm644 -t /usr/share/qrec/fonts "$cache"/x/*.ttf
	install -Dm644 -t /usr/share/licenses/qrec-fonts /src/crates/qrec-gui/resources/fonts/OFL-*.txt
}

drop_gst_plugins() {
	for plugin in $DROP_GST_PLUGINS; do
		rm -fv /usr/lib/gstreamer-1.0/"$plugin"
	done
}

bundle() {
	log "Bundling with quick-sharun ($ANYLINUX_COMMIT)"
	mkdir -p /work/cache /work/out /work/build
	fetch "$PREFIX_URL/quick-sharun.sh" /work/cache/quick-sharun-"$ANYLINUX_COMMIT".sh "$QUICK_SHARUN_SHA256"
	cp /work/cache/quick-sharun-"$ANYLINUX_COMMIT".sh /usr/local/bin/quick-sharun
	chmod +x /usr/local/bin/quick-sharun

	name=Qrec-$QREC_VERSION-x86_64.AppImage
	rm -rf /work/build/AppDir /work/out/"$name"
	export APPDIR=/work/build/AppDir OUTPATH=/work/out OUTNAME="$name" VERSION="$QREC_VERSION"
	export DESKTOP=/usr/share/applications/com.rsensato.qrec.desktop
	export ICON=/usr/share/icons/hicolor/256x256/apps/com.rsensato.qrec.png
	export MAIN_BIN=qrec
	export DEPLOY_GTK=1 DEPLOY_GDK=1 DEPLOY_OPENGL=1 DEPLOY_VULKAN=1
	export DEPLOY_GSTREAMER=1 DEPLOY_PIPEWIRE=1 DEPLOY_PULSE=1
	# The dlopen survey (LD_DEBUG=libs under xvfb-run): the window on a new
	# project with the test-pattern sources, long enough for GStreamer's
	# first registry scan.
	mkdir -p /tmp/qrec-survey
	export STRACE_BINARY=qrec STRACE_FLAGS="--project /tmp/qrec-survey" STRACE_TIME=25
	export QREC_CAPTURE_BACKEND=fake XDG_RUNTIME_DIR=/tmp/qrec-runtime HOME=/tmp/qrec-home
	mkdir -p "$XDG_RUNTIME_DIR" "$HOME" && chmod 700 "$XDG_RUNTIME_DIR"
	# A session bus of the container's own, as a desktop has.
	DBUS_SESSION_BUS_ADDRESS=$(dbus-daemon --session --fork --print-address)
	export DBUS_SESSION_BUS_ADDRESS

	# The binaries Qrec runs (docs/ARCHITECTURE.md): ffmpeg and ffprobe for
	# probes and exports, pactl for audio sources, wl-copy for Copy image
	# after quitting. Qrec runs nothing from v4l-utils.
	cd /work/build
	quick-sharun /usr/bin/qrec /usr/bin/ffmpeg /usr/bin/ffprobe /usr/bin/pactl /usr/bin/wl-copy

	tune_appdir
	quick-sharun --make-appimage
	patch_runtime "/work/out/$name"
	sha256sum "/work/out/$name" | sed 's|/work/out/||' > "/work/out/$name.sha256"
	log "$(du -h "/work/out/$name" | cut -f1) /work/out/$name"
}

# What Qrec's environment needs beyond quick-sharun's defaults.
tune_appdir() {
	for plugin in $DROP_GST_PLUGINS; do
		[ ! -e "$APPDIR/lib/gstreamer-1.0/$plugin" ] || die "$plugin was bundled"
	done
	for font in "Fraunces[SOFT,WONK,opsz,wght].ttf" "AtkinsonHyperlegibleNext[wght].ttf" "JetBrainsMono[wght].ttf"; do
		[ -f "$APPDIR/share/qrec/fonts/$font" ] || die "$font is missing from share/qrec/fonts"
	done
	# quick-sharun rewrites /usr/share and /usr/lib in every binary that
	# names them, for its /tmp links into the AppImage. In Qrec those are
	# lookups of the host's (/usr/share/zoneinfo, the VA driver directories,
	# os-release), so qrec goes back unpatched, stripped as quick-sharun does.
	strip -s -R .comment --strip-unneeded -o "$APPDIR/shared/bin/qrec" /usr/bin/qrec
	chmod 755 "$APPDIR/shared/bin/qrec"
	# quick-sharun bundles only the hicolor icon theme; the flake's wrapper
	# puts Adwaita's on XDG_DATA_DIRS too (GTK's and libadwaita's icons, and
	# its cursors).
	[ -d "$APPDIR/share/icons/Adwaita" ] || cp -r /usr/share/icons/Adwaita "$APPDIR/share/icons/"
	cat >> "$APPDIR/.env" <<-'EOF'
	# Qrec (packaging/appimage/build.sh). Only the bundled GStreamer plugins,
	# as the flake's wrapper does: a session's plugin paths (NixOS sets one
	# with an unfiltered gst-plugins-bad) are never scanned.
	GST_PLUGIN_SYSTEM_PATH_1_0=${SHARUN_DIR}/lib/gstreamer-1.0
	GST_PLUGIN_SYSTEM_PATH=${SHARUN_DIR}/lib/gstreamer-1.0
	unset GST_PLUGIN_PATH GST_PLUGIN_PATH_1_0 GST_PLUGIN_SCANNER_1_0
	EOF
	# AppRun sources bin/*.hook before it starts Qrec. This one applies
	# Qrec's EGL policy (qrec-shell/src/gpu.rs, A§8.10), which Qrec can't
	# apply from inside the AppImage yet: sharun would put NVIDIA's vendor
	# first whenever its kernel module is loaded, which Qrec then takes for
	# the user's choice, and Qrec's own pin would take NixOS's
	# /run/opengl-driver Mesa, a host library among the bundled ones.
	cat > "$APPDIR/bin/50-qrec-egl.hook" <<-'EOF'
	#!/bin/sh
	# Qrec (packaging/appimage/build.sh), unless __EGL_VENDOR_LIBRARY_FILENAMES
	# names a vendor: with an Intel or AMD GPU, EGL loads the bundled Mesa
	# only, never NVIDIA's or the host's Mesa; with NVIDIA only, sharun puts
	# the host's NVIDIA driver first; without a GPU, the bundled Mesa draws
	# in software.
	if [ -z "${__EGL_VENDOR_LIBRARY_FILENAMES:-}" ] \
	        && [ -f "$APPDIR/share/glvnd/egl_vendor.d/50_mesa.json" ]; then
	        for _qrec_vendor in /sys/class/drm/renderD*/device/vendor; do
	                case "$(cat "$_qrec_vendor" 2>/dev/null)" in
	                        0x8086|0x1002)
	                                export SHARUN_NO_NVIDIA_EGL_PRIME=1
	                                export __EGL_VENDOR_LIBRARY_FILENAMES="$APPDIR/share/glvnd/egl_vendor.d/50_mesa.json"
	                                break
	                                ;;
	                esac
	        done
	        unset _qrec_vendor
	fi
	EOF
	# What went in, for bug reports.
	{
		printf 'Qrec %s, built %s\n' "$QREC_VERSION" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
		printf 'Arch Linux Archive %s, %s\n' "$(cat /opt/qrec-appimage/snapshot)" "$ARCH_IMAGE"
		printf 'quick-sharun at pkgforge-dev/Anylinux-AppImages %s\n' "$ANYLINUX_COMMIT"
		printf 'sharun %s\n' "$("$APPDIR"/sharun --version 2>/dev/null || echo '?')"
		printf 'Debloated packages (pkgforge-dev/archlinux-pkgs-debloated):\n'
		sed 's/^/  /' /opt/qrec-appimage/debloated.txt
		printf 'Packages:\n'
		pacman -Q | sed 's/^/  /'
	} > "$APPDIR/share/qrec/appimage-build.txt"
	cp "$APPDIR/share/qrec/appimage-build.txt" "/work/out/$OUTNAME.build.txt"
}

# uruntime falls back to extracting without FUSE only for images up to
# 350 MiB (URUNTIME_EXTRACT=3); 2 extracts whatever the size. The policy is a
# fixed-width string in the runtime, before the image.
patch_runtime() {
	offsets=$(head -c 16777216 "$1" | grep -b -a -o 'URUNTIME_EXTRACT=[0-9]' | cut -d: -f1)
	[ "$(printf '%s\n' "$offsets" | grep -c .)" = 1 ] \
		|| die "expected one URUNTIME_EXTRACT policy in the runtime, found: $offsets"
	offset=$offsets
	printf 2 | dd of="$1" bs=1 seek=$((offset + 17)) conv=notrunc status=none
	head -c 16777216 "$1" | grep -a -o 'URUNTIME_EXTRACT=[0-9]'
}

in_container() {
	case "$1" in
		--prepare-builder)
			prepare_builder
			;;
		--bundle)
			trap 'chown -R "$HOST_UID:$HOST_GID" /work' EXIT
			[ "$(cat /opt/qrec-appimage/snapshot)" = "$ARCH_SNAPSHOT" ] \
				|| die "the builder image is for another snapshot"
			build_qrec
			install_fonts
			drop_gst_plugins
			bundle
			;;
	esac
}

# --- On the host -------------------------------------------------------------

host() {
	here=$(cd "$(dirname "$0")" && pwd)
	repo=$(cd "$here/../.." && pwd)
	work=${QREC_APPIMAGE_DIR:-$repo/tmp/appimage}
	builder=qrec-appimage-builder:$(printf '%s' "$ARCH_SNAPSHOT" | tr / -)-r$BUILDER_REVISION
	command -v docker >/dev/null || die "docker is needed"
	mkdir -p "$work/logs"

	if [ "${1:-}" = --builder ] || ! docker image inspect "$builder" >/dev/null 2>&1; then
		log "Making the builder image $builder (Arch Linux Archive $ARCH_SNAPSHOT)"
		docker rm -f qrec-appimage-prepare >/dev/null 2>&1 || :
		# nice inside the container: the CLI's nice wouldn't reach it.
		docker run --name qrec-appimage-prepare --cpus="$JOBS" \
			-v "$here/build.sh:/opt/qrec-build.sh:ro" \
			-e ARCH_SNAPSHOT="$ARCH_SNAPSHOT" \
			"$ARCH_IMAGE" nice -n 19 sh /opt/qrec-build.sh --prepare-builder
		docker commit --change 'CMD ["/bin/bash"]' qrec-appimage-prepare "$builder" >/dev/null
		docker rm qrec-appimage-prepare >/dev/null
		[ "${1:-}" != --builder ] || exit 0
	fi

	version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -n 1)
	if rev=$(git -C "$repo" rev-parse --short HEAD 2>/dev/null); then
		git -C "$repo" diff --quiet HEAD 2>/dev/null || rev=$rev-dirty
		version=$version-$rev
	fi
	log "Building Qrec $version in $builder"
	docker run --rm --name "qrec-appimage-build-$$" --cpus="$JOBS" \
		-v "$repo:/src:ro" -v "$work:/work" \
		-v "$here/build.sh:/opt/qrec-build.sh:ro" \
		-e ARCH_SNAPSHOT="$ARCH_SNAPSHOT" -e JOBS="$JOBS" -e QREC_VERSION="$version" \
		-e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
		"$builder" nice -n 19 sh /opt/qrec-build.sh --bundle
}

if [ "${1:-}" = --prepare-builder ] || [ "${1:-}" = --bundle ]; then
	in_container "$1"
else
	host "$@"
fi
