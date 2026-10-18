#!/usr/bin/env bash
# Installs "apifa" as a desktop application for the current user.
# macOS: ~/Applications/apifa.app (+ the command in ~/.local/bin).
# NixOS / Nix: installed in your Nix profile. Other Linux: ~/.local (no root needed).
set -euo pipefail
cd "$(dirname "$0")"
APP=apifa
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"

# Desktop entry + icons in ~/.local/share (watched live by GNOME, KDE…).
install_launcher() {
    local exec="$1"
    for s in 48 128 256 512; do
        install -Dm644 "assets/icons/$s.png" "$DATA/icons/hicolor/${s}x${s}/apps/$APP.png"
    done
    mkdir -p "$DATA/applications"
    cat > "$DATA/applications/$APP.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=apifa
GenericName=API Client
Comment=API For All — HTTP, GraphQL, WebSocket and gRPC client
Keywords=api;rest;http;graphql;websocket;grpc;postman;bruno;
Exec=$exec
Icon=$APP
Categories=Development;
StartupWMClass=$APP
Terminal=false
DESKTOP
    chmod 644 "$DATA/applications/$APP.desktop"
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$DATA/applications" || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$DATA/icons/hicolor" || true
}

# macOS: an .app bundle in ~/Applications (shows up in Launchpad / Spotlight).
install_macos() {
    command -v cargo >/dev/null 2>&1 || { echo "cargo is required: https://rustup.rs"; exit 1; }
    echo "→ Building…"
    cargo build --release
    local app="$HOME/Applications/apifa.app"
    rm -rf "$app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp "target/release/$APP" "$app/Contents/MacOS/$APP"
    chmod 755 "$app/Contents/MacOS/$APP"

    local tmp
    tmp=$(mktemp -d)
    local set="$tmp/$APP.iconset"
    mkdir -p "$set"
    for s in 16 32 128 256 512; do
        sips -z $s $s assets/icons/512.png --out "$set/icon_${s}x${s}.png" >/dev/null
        local d=$((s * 2))
        if [ $d -le 512 ]; then
            sips -z $d $d assets/icons/512.png --out "$set/icon_${s}x${s}@2x.png" >/dev/null
        fi
    done
    iconutil -c icns "$set" -o "$app/Contents/Resources/$APP.icns"
    rm -rf "$tmp"

    local version
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
    cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>apifa</string>
    <key>CFBundleDisplayName</key><string>apifa</string>
    <key>CFBundleIdentifier</key><string>io.apifa.app</string>
    <key>CFBundleExecutable</key><string>$APP</string>
    <key>CFBundleIconFile</key><string>$APP</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key><string>10.13</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
    # Ad-hoc signature (needed on Apple Silicon once the binary is copied), then tell Launch Services.
    codesign --force --sign - "$app" >/dev/null 2>&1 || true
    /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$app" >/dev/null 2>&1 || true

    # The command line (apifa run / check / …).
    local bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
    mkdir -p "$bin"
    ln -sf "$app/Contents/MacOS/$APP" "$bin/$APP"
    echo "✓ Installed in ~/Applications — open “apifa” from Launchpad or Spotlight."
    case ":$PATH:" in
        *":$bin:"*) ;;
        *) echo "  (add $bin to your PATH to use the \`$APP\` command)" ;;
    esac
}

if [ "$(uname -s)" = Darwin ]; then
    install_macos
    exit 0
fi

if command -v nix-build >/dev/null 2>&1; then
    echo "→ Building with Nix…"
    out=$(nix-build --no-out-link default.nix)
    if nix profile list >/dev/null 2>&1; then
        # New-style profile ("nix profile"); replace any previous install.
        nix profile remove "$APP" >/dev/null 2>&1 || true
        nix profile add "$out" 2>/dev/null || nix profile install "$out"
    else
        nix-env -i "$out"
    fi
    # GNOME only watches application folders that existed when the session started:
    # also register the launcher where it is always watched, so search finds it right away.
    install_launcher "$HOME/.nix-profile/bin/$APP"
else
    command -v cargo >/dev/null 2>&1 || { echo "cargo is required: https://rustup.rs"; exit 1; }
    echo "→ Building…"
    cargo build --release
    BIN="${XDG_BIN_HOME:-$HOME/.local/bin}"
    install -Dm755 "target/release/$APP" "$BIN/$APP"
    install_launcher "$BIN/$APP"
fi
echo "✓ Installed — open “apifa” from your applications menu."
