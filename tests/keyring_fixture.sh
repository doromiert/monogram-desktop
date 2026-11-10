#!/usr/bin/env bash
set -euo pipefail
fixture_test=${1:?Rust unit-test binary required}
fixture_test=$(realpath "$fixture_test")
fixture_bus_config=${2:-/etc/dbus-1/session.conf}
fixture_filter=${3:-crypto::vault::tests::isolated_secret_service}
fixture_root=$(mktemp -d)
trap 'rm -rf "$fixture_root"' EXIT
mkdir -m 700 "$fixture_root"/{home,config,data,cache,state,run}
# Keep the upstream bus policy/authentication, but disable unrelated desktop activation.
python3 - "$fixture_bus_config" "$fixture_root/session.conf" "$fixture_root/run" <<'PY'
import sys
import xml.etree.ElementTree as ET
tree = ET.parse(sys.argv[1])
root = tree.getroot()
for child in list(root):
    if child.tag in {"standard_session_servicedirs", "servicedir", "includedir", "include", "fork", "pidfile", "servicehelper"}:
        root.remove(child)
    elif child.tag == "listen":
        child.text = "unix:tmpdir=" + sys.argv[3]
tree.write(sys.argv[2], encoding="utf-8", xml_declaration=True)
PY
# A new bus and XDG roots isolate every secret from the desktop login/keyring.
env HOME="$fixture_root/home" XDG_CONFIG_HOME="$fixture_root/config" \
  XDG_DATA_HOME="$fixture_root/data" XDG_CACHE_HOME="$fixture_root/cache" \
  XDG_STATE_HOME="$fixture_root/state" XDG_RUNTIME_DIR="$fixture_root/run" \
  GIO_USE_VFS=local GSETTINGS_BACKEND=memory GTK_USE_PORTAL=0 \
  MONOGRAM_KEYRING_FIXTURE=1 dbus-run-session --config-file="$fixture_root/session.conf" -- bash -c '
    set -euo pipefail
    cd "$HOME"
    unset GNOME_KEYRING_CONTROL GNOME_KEYRING_PID SSH_AUTH_SOCK
    printf "%s" "fictional fixture password" | gnome-keyring-daemon --foreground --unlock --components=secrets --control-directory="$XDG_RUNTIME_DIR/keyring" >"$XDG_RUNTIME_DIR/keyring.log" 2>&1 &
    fixture_daemon=$!
    trap '\''kill "$fixture_daemon" 2>/dev/null || true; wait "$fixture_daemon" 2>/dev/null || true'\'' EXIT
    for _ in {1..100}; do
      if dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.NameHasOwner string:org.freedesktop.secrets | grep -q "boolean true"; then break; fi
      kill -0 "$fixture_daemon" || { cat "$XDG_RUNTIME_DIR/keyring.log"; exit 1; }
      sleep 0.05
    done
    if [[ "$2" == --* ]]; then "$1" "$2"; else "$1" --ignored --exact "$2" --nocapture; fi
  ' bash "$fixture_test" "$fixture_filter"
