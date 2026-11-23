#!/bin/sh
# Run the display-needing (ignored) GTK tests without a visible display or your desktop session.
#
#     tools/headless-gtk-test.sh <cargo test args>
#     tools/headless-gtk-test.sh --bin monogram-desktop -- --ignored the_dialog_crops
#
# Nothing appears on your screen and the tests get their own private D-Bus session. Backends, in
# the order they are auto-detected:
#   kwin      KWin's virtual compositor: a real Wayland compositor with no output, so GTK gets the
#             frame clock and mapping that row rendering and animation need.
#   weston    Weston's headless backend with the pixman renderer: the same idea without KWin, and
#             what NixOS (default.nix) uses. No GPU, no input device, no X server.
#   broadway  GTK's in-memory Broadway display. Fine for widget-only tests, but with no viewer
#             connected it never ticks the frame clock, so timeline rendering stalls.
# Configuration is isolated too: HOME, XDG_* and the runtime dir point at a temp sandbox that is
# deleted afterwards, so your real settings are never read or modified.
# Set HEADLESS_BACKEND=kwin|weston|broadway to force one. Only the tests that drive input through
# xdotool still need X11; they are the remaining holdouts (see docs/TESTING.md).
set -eu
# Everything below, the compositor included, runs inside a private D-Bus session, so nothing
# registers on or talks to your real session bus.
# The private bus must not start anything on demand. With the stock session configuration it
# activates desktop services (xdg-desktop-portal, ksecretd, ...) through your user systemd, and
# those outlive the run: hundreds of idle processes after a few hundred runs. So the bus gets the
# system session policy with every service directory removed, as tests/keyring_fixture.sh does;
# a client that asks for a service that is not there just does without it.
# Anything else that did get started would also keep the log pipe open and hang whoever reads it,
# so the session's output goes to a log file, which is printed once the session is gone; the exit
# status is the tests'.
if [ -z "${HEADLESS_INNER:-}" ]; then
    log="$(mktemp "${TMPDIR:-/tmp}/headless-gtk.XXXXXX")"
    conf="$(mktemp "${TMPDIR:-/tmp}/headless-gtk-bus.XXXXXX")"
    status=0
    # The stock session policy: /etc on most systems, /usr/share on Arch. HEADLESS_BUS_CONFIG
    # overrides it. Without one the run is refused: a bus that can start services leaks processes.
    source_config=""
    for candidate in "${HEADLESS_BUS_CONFIG:-}" /etc/dbus-1/session.conf /usr/share/dbus-1/session.conf; do
        if [ -n "$candidate" ] && [ -r "$candidate" ]; then
            source_config="$candidate"
            break
        fi
    done
    if [ -z "$source_config" ] || ! python3 -I - "$source_config" "$conf" <<'PY'
import sys
import xml.etree.ElementTree as ET
tree = ET.parse(sys.argv[1])
root = tree.getroot()
for child in list(root):
    if child.tag in {"standard_session_servicedirs", "servicedir", "includedir", "include", "servicehelper"}:
        root.remove(child)
tree.write(sys.argv[2], encoding="utf-8", xml_declaration=True)
PY
    then
        rm -f "$log" "$conf"
        echo "headless-gtk-test: no D-Bus session.conf found to derive a no-activation bus from." >&2
        echo "Set HEADLESS_BUS_CONFIG=/path/to/session.conf. Refusing to run: the stock bus starts" >&2
        echo "desktop services (portals, ksecretd) that are never cleaned up." >&2
        exit 1
    fi
    HEADLESS_INNER=1 HEADLESS_LOG="$log" dbus-run-session --config-file="$conf" -- "$0" "$@" >/dev/null 2>&1 </dev/null || status=$?
    cat "$log"
    rm -f "$log" "$conf"
    exit "$status"
fi
# Isolate configuration: KWin, GTK, GSettings and the app itself get a throwaway HOME and XDG tree
# (and a private runtime dir for the Wayland socket), so nothing reads or writes your real
# ~/.config, ~/.local/share, ~/.cache or $XDG_RUNTIME_DIR. Cargo and rustup keep using the real
# toolchain and registry through explicit CARGO_HOME/RUSTUP_HOME. The build dir is the project's.
real_home="${HOME:-}"
export CARGO_HOME="${CARGO_HOME:-$real_home/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$real_home/.rustup}"
sandbox="$(mktemp -d "${TMPDIR:-/tmp}/headless-gtk-home.XXXXXX")"
mkdir -p "$sandbox/home" "$sandbox/config" "$sandbox/data" "$sandbox/cache" "$sandbox/state" "$sandbox/runtime"
chmod 700 "$sandbox/runtime"
export HOME="$sandbox/home" XDG_CONFIG_HOME="$sandbox/config" XDG_DATA_HOME="$sandbox/data" \
    XDG_CACHE_HOME="$sandbox/cache" XDG_STATE_HOME="$sandbox/state" XDG_RUNTIME_DIR="$sandbox/runtime" \
    GSETTINGS_BACKEND=memory
unset XDG_CONFIG_DIRS SSH_AUTH_SOCK GNOME_KEYRING_CONTROL KDE_FULL_SESSION KDE_SESSION_VERSION
backend="${HEADLESS_BACKEND:-}"
if [ -z "$backend" ]; then
    if command -v kwin_wayland >/dev/null 2>&1; then backend=kwin
    elif command -v weston >/dev/null 2>&1; then backend=weston
    else backend=broadway; fi
fi
runtime="${XDG_RUNTIME_DIR:-/tmp}"
case "$backend" in
kwin)
    socket="monogram-headless-$$"
    kwin_wayland --virtual --socket "$socket" --width 1600 --height 1000 --no-lockscreen \
        >/dev/null 2>&1 &
    compositor=$!
    i=0
    while [ ! -S "$runtime/$socket" ] && [ "$i" -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
    [ -S "$runtime/$socket" ] || { echo "kwin_wayland --virtual did not start" >&2; exit 1; }
    gdk="GDK_BACKEND=wayland WAYLAND_DISPLAY=$socket"
    ;;
weston)
    socket="monogram-headless-$$"
    weston --backend=headless --renderer=pixman --socket="$socket" --width=1600 --height=1000 \
        >/dev/null 2>&1 &
    compositor=$!
    i=0
    while [ ! -S "$runtime/$socket" ] && [ "$i" -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
    [ -S "$runtime/$socket" ] || { echo "weston --backend=headless did not start" >&2; exit 1; }
    gdk="GDK_BACKEND=wayland WAYLAND_DISPLAY=$socket GSK_RENDERER=cairo"
    ;;
broadway)
    display=":${BROADWAY_TEST_DISPLAY:-47}"
    gtk4-broadwayd "$display" >/dev/null 2>&1 &
    compositor=$!
    sleep 1
    gdk="GDK_BACKEND=broadway BROADWAY_DISPLAY=$display GSK_RENDERER=cairo"
    ;;
*) echo "unknown HEADLESS_BACKEND '$backend'" >&2; exit 1 ;;
esac
trap 'kill "$compositor" 2>/dev/null || true; rm -rf "$sandbox"' EXIT INT TERM
status=0
# shellcheck disable=SC2086
env -u DISPLAY -u XDG_SESSION_TYPE $gdk GTK_A11Y=none NO_AT_BRIDGE=1 cargo test "$@" >"$HEADLESS_LOG" 2>&1 || status=$?
exit "$status"
