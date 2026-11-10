#!/usr/bin/env bash
set -euo pipefail
fixture_test=${1:?Rust unit-test binary required}
fixture_bus_config=${2:-/etc/dbus-1/session.conf}
fixture_root=$(mktemp -d)
trap 'rm -rf "$fixture_root"' EXIT
mkdir -m 700 "$fixture_root"/{home,config,data,cache,state,run}
# A new bus and XDG roots isolate every secret from the desktop login/keyring.
env HOME="$fixture_root/home" XDG_CONFIG_HOME="$fixture_root/config" \
  XDG_DATA_HOME="$fixture_root/data" XDG_CACHE_HOME="$fixture_root/cache" \
  XDG_STATE_HOME="$fixture_root/state" XDG_RUNTIME_DIR="$fixture_root/run" \
  MONOGRAM_KEYRING_FIXTURE=1 dbus-run-session --config-file="$fixture_bus_config" -- bash -c '
    set -euo pipefail
    unset GNOME_KEYRING_CONTROL GNOME_KEYRING_PID SSH_AUTH_SOCK
    printf "%s" "fictional fixture password" | gnome-keyring-daemon --foreground --unlock --components=secrets --control-directory="$XDG_RUNTIME_DIR/keyring" >"$XDG_RUNTIME_DIR/keyring.log" 2>&1 &
    fixture_daemon=$!
    trap '\''kill "$fixture_daemon" 2>/dev/null || true; wait "$fixture_daemon" 2>/dev/null || true'\'' EXIT
    for _ in {1..100}; do
      if dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.NameHasOwner string:org.freedesktop.secrets | grep -q "boolean true"; then break; fi
      kill -0 "$fixture_daemon" || { cat "$XDG_RUNTIME_DIR/keyring.log"; exit 1; }
      sleep 0.05
    done
    "$1" --ignored --exact crypto::vault::tests::isolated_secret_service --nocapture
  ' bash "$fixture_test"
