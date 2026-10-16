{
  description = "Qrec - quick Wayland screen and camera recorder (press R, talk, press R)";

  inputs = {
    # Pinned to the same revision as the dev machine's system so most of the
    # closure is already in the store.
    nixpkgs.url = "github:NixOS/nixpkgs/4c7870105e7f1fdf9c48688c8d7efc21abf0688a";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # gst-plugins-bad without the plugins that hang the registry scan.
      # nvcodec (CUDA) and qsv (oneVPL) probe every GPU inside
      # gst-plugin-scanner; on the dev machine the NVIDIA driver on
      # renderD128 can wedge, and then every gst::init() blocks forever.
      # Qrec encodes with VA-API on the Intel node instead (docs/RECORDING.md).
      gstPluginsBadFiltered =
        pkgs:
        pkgs.runCommand "gst-plugins-bad-qrec-${pkgs.gst_all_1.gst-plugins-bad.version}" { } ''
          mkdir -p $out/lib/gstreamer-1.0
          for f in ${pkgs.gst_all_1.gst-plugins-bad}/lib/gstreamer-1.0/*; do
            case "$(basename "$f")" in
              libgstnvcodec.so | libgstqsv.so) ;;
              *) ln -s "$f" $out/lib/gstreamer-1.0/ ;;
            esac
          done
        '';

      # gst-editing-services propagates the unfiltered gst-plugins-bad, so its
      # plugin dir is dropped from the search path again here.
      filterGstPluginPath = ''
        GST_PLUGIN_SYSTEM_PATH_1_0="$(printf '%s' "''${GST_PLUGIN_SYSTEM_PATH_1_0:-}" | tr ':' '\n' \
          | grep -v -E -- '-gst-plugins-bad-[0-9.]+/lib/gstreamer-1.0$' | paste -sd: -)"
        export GST_PLUGIN_SYSTEM_PATH_1_0
      '';

      gstPlugins =
        pkgs:
        (with pkgs.gst_all_1; [
          gstreamer
          gst-plugins-base
          gst-plugins-good # v4l2src, pulsesrc, matroskamux, flacenc, level
          gst-plugins-ugly # x264enc
          gst-plugins-rs # gtk4paintablesink
          gst-libav
          gst-editing-services
        ])
        ++ [ (gstPluginsBadFiltered pkgs) ]; # va (vah264enc), compositor

      # Libraries the Rust crates link against (via pkg-config).
      linkLibs =
        pkgs:
        (gstPlugins pkgs)
        ++ (with pkgs; [
          glib
          gtk4
          libadwaita
          dbus
          pipewire # pipewiresrc GStreamer plugin
          wayland
        ]);

      # Tools Qrec spawns at runtime.
      runtimeTools =
        pkgs: with pkgs; [
          ffmpeg-full
          wl-clipboard
          pulseaudio # pactl client only; the sound server is PipeWire
          v4l-utils
        ];

      # The fonts bundled with the GUI (SPEC §15.3, docs/GUI.md G§6.3): the
      # three variable fonts, found through $QREC_FONT_DIR in the dev shell
      # and <exe>/../share/qrec/fonts in the package, with their OFL texts
      # (vendored: the font packages don't install them).
      qrecFonts =
        pkgs:
        pkgs.runCommand "qrec-fonts" { } ''
          mkdir -p $out/share/qrec/fonts $out/share/licenses/qrec-fonts
          cp '${pkgs.fraunces}/share/fonts/truetype/Fraunces[SOFT,WONK,opsz,wght].ttf' \
             '${pkgs.atkinson-hyperlegible-next}/share/fonts/truetype/AtkinsonHyperlegibleNext[wght].ttf' \
             '${pkgs.jetbrains-mono}/share/fonts/truetype/JetBrainsMono[wght].ttf' \
             $out/share/qrec/fonts/
          cp ${./crates/qrec-gui/resources/fonts}/OFL-*.txt $out/share/licenses/qrec-fonts/
        '';

      # What the window needs at runtime besides GTK: icon themes and the
      # GSettings schemas (wrapGAppsHook4 puts them on XDG_DATA_DIRS for the
      # package), and librsvg (SVG icons; rsvg-convert for the art PNGs).
      guiData =
        pkgs: with pkgs; [
          adwaita-icon-theme
          hicolor-icon-theme
          gsettings-desktop-schemas
          librsvg
        ];
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            cargo-insta
            cargo-nextest
            pkg-config
            wrapGAppsHook4
            # The GUI tests' nested compositor, screenshots and key presses
            # (docs/GUI.md G§8.4); glib-compile-resources comes with glib.
            sway
            grim
            wtype
          ];
          buildInputs = (linkLibs pkgs) ++ (runtimeTools pkgs) ++ (guiData pkgs);
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          shellHook = filterGstPluginPath + ''
            # A registry of its own, so switching between this shell and the
            # system GStreamer doesn't trigger a full plugin rescan each time.
            export GST_REGISTRY_1_0="''${XDG_CACHE_HOME:-$HOME/.cache}/qrec-dev/gstreamer-registry.bin"
            # The lv2 plugin otherwise scans every LV2 plugin on the machine.
            export LV2_PATH="''${XDG_CACHE_HOME:-$HOME/.cache}/qrec-dev/no-lv2"
            # The va plugin initializes libva on every render node during the
            # registry scan. On hybrid Intel+NVIDIA machines that loads the
            # NVIDIA VA driver, which can hang. Pin libva to Intel's driver
            # when an Intel node exists (Qrec does the same at runtime).
            if [ -z "''${LIBVA_DRIVER_NAME:-}" ]; then
              for node in /sys/class/drm/renderD*; do
                if [ "$(cat "$node/device/vendor" 2>/dev/null)" = "0x8086" ]; then
                  export LIBVA_DRIVER_NAME=iHD
                  break
                fi
              done
            fi
            # The GUI's bundled fonts (the package installs them next to the
            # binary instead).
            export QREC_FONT_DIR=${qrecFonts pkgs}/share/qrec/fonts
            # GSettings schemas and icon themes, as wrapGAppsHook4 does for
            # the package.
            export XDG_DATA_DIRS="''${GSETTINGS_SCHEMAS_PATH:-}:${pkgs.adwaita-icon-theme}/share:${pkgs.hicolor-icon-theme}/share:''${XDG_DATA_DIRS:-}"
          '';
        };
      });

      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "qrec";
          version = "0.1.0";
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              let
                base = baseNameOf path;
              in
              !(builtins.elem base [ "target" "result" ".direnv" ".claude" "design" ]);
          };
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [
            pkg-config
            wrapGAppsHook4
            makeWrapper
            glib # glib-compile-resources (qrec-gui's build.rs)
            librsvg # rsvg-convert (icon PNGs)
          ];
          buildInputs = (linkLibs pkgs) ++ (guiData pkgs);
          # Integration tests need a session bus, devices and ffmpeg; run the
          # pure core tests only inside the sandbox.
          cargoTestFlags = [ "-p" "qrec-core" ];
          # Runs before wrapGAppsHook captures the plugin path.
          preBuild = filterGstPluginPath;
          # The GUI's fonts next to the binary (<exe>/../share/qrec/fonts)
          # and their licenses.
          postInstall = ''
            mkdir -p $out/share
            cp -r ${qrecFonts pkgs}/share/qrec $out/share/
            cp -r ${qrecFonts pkgs}/share/licenses $out/share/
          '';
          preFixup = ''
            gappsWrapperArgs+=(--prefix PATH : ${pkgs.lib.makeBinPath (runtimeTools pkgs)})
          '';
          meta = {
            description = "Quick Wayland screen and camera recorder with a built-in editor";
            mainProgram = "qrec";
            platforms = pkgs.lib.platforms.linux;
          };
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
