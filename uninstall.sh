#!/usr/bin/env bash
# Removes "apifa". Add --purge to also delete your data (credentials, workspace, history).
set -uo pipefail
APP=apifa

if [ "$(uname -s)" = Darwin ]; then
    rm -rf "$HOME/Applications/apifa.app"
    CONFIG="$HOME/Library/Application Support/$APP"
else
    CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/$APP"
fi

if command -v nix >/dev/null 2>&1 && nix profile list 2>/dev/null | grep -q "$APP"; then
    nix profile remove "$APP"
elif command -v nix-env >/dev/null 2>&1 && nix-env -q "$APP" >/dev/null 2>&1; then
    nix-env -e "$APP"
fi
BIN="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
rm -f "$BIN/$APP" "$DATA/applications/$APP.desktop"
for s in 48 128 256 512; do rm -f "$DATA/icons/hicolor/${s}x${s}/apps/$APP.png"; done

if [ "${1:-}" = "--purge" ]; then
    # The auth secrets and the AI keys are in the system keychain (one entry per data folder).
    if [ "$(uname -s)" = Darwin ]; then
        security delete-generic-password -s "$APP" -a "secrets $CONFIG" >/dev/null 2>&1 || true
    elif command -v secret-tool >/dev/null 2>&1; then
        secret-tool clear service "$APP" >/dev/null 2>&1 || true
    fi
    rm -rf "$CONFIG"
    echo "✓ Uninstalled, data deleted."
else
    echo "✓ Uninstalled. Your data is kept in $CONFIG (use --purge to delete it)."
fi
