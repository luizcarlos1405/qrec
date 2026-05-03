{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = with pkgs; [
    rustc
    cargo
    rustfmt
    clippy
    wf-recorder
    ffmpeg
    mpv
    pulseaudio
  ];
}
