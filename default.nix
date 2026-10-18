# Installable package: `nix-build` (result/bin/apifa) or `nix-env -if .`
{ pkgs ? import <nixpkgs> { } }:
let
  runtimeLibs = with pkgs; [
    wayland
    libxkbcommon
    libGL
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
  ];
in
pkgs.rustPlatform.buildRustPackage {
  pname = "apifa";
  version = "0.1.0";
  src = pkgs.lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = with pkgs; [ pkg-config makeWrapper copyDesktopItems ];
  buildInputs = runtimeLibs;

  desktopItems = [
    (pkgs.makeDesktopItem {
      name = "apifa";
      exec = "apifa";
      icon = "apifa";
      desktopName = "apifa";
      comment = "API For All — HTTP, GraphQL, WebSocket and gRPC client";
      categories = [ "Development" ];
      startupWMClass = "apifa";
    })
  ];

  postInstall = ''
    for s in 48 128 256 512; do
      install -Dm644 assets/icons/$s.png $out/share/icons/hicolor/''${s}x''${s}/apps/apifa.png
    done
    wrapProgram $out/bin/apifa \
      --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibs}
  '';
}
