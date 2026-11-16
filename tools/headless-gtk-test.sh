#!/bin/sh
# Run the display-needing (ignored) GTK tests without a visible display or your desktop session.
#
#     tools/headless-gtk-test.sh <cargo test args>
#     tools/headless-gtk-test.sh --bin monogram-desktop -- --ignored the_dialog_crops
#
# Nothing appears on your screen and the tests get their own private D-Bus session. Two backends:
#   kwin      (default when installed) KWin's virtual compositor: a real Wayland compositor with no
#             output, so GTK gets the frame clock and mapping that row rendering and animation need.
#   broadway  GTK's in-memory Broadway display. Fine for widget-only tests, but with no viewer
#             connected it never ticks the frame clock, so timeline rendering stalls.
# Set HEADLESS_BACKEND=broadway to force the second. Tests that need xdotool, a keyring or an X11
# 60 Hz clock still need a real isolated X server (see default.nix).
set -eu
# Everything below, the compositor included, runs inside a private D-Bus session, so nothing
# registers on or talks to your real session bus.
# Programs the private bus starts on demand can outlive this script and keep its output pipe open,
# which would hang whoever is reading it. So the session's output goes to a log file, which is
# printed once the session is gone; the exit status is the tests'.
if [ -z "${HEADLESS_INNER:-}" ]; then
    log="$(mktemp "${TMPDIR:-/tmp}/headless-gtk.XXXXXX")"
    status=0
    HEADLESS_INNER=1 HEADLESS_LOG="$log" dbus-run-session -- "$0" "$@" >/dev/null 2>&1 </dev/null || status=$?
    cat "$log"
    rm -f "$log"
    exit "$status"
fi
backend="${HEADLESS_BACKEND:-}"
if [ -z "$backend" ]; then
    if command -v kwin_wayland >/dev/null 2>&1; then backend=kwin; else backend=broadway; fi
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
broadway)
    display=":${BROADWAY_TEST_DISPLAY:-47}"
    gtk4-broadwayd "$display" >/dev/null 2>&1 &
    compositor=$!
    sleep 1
    gdk="GDK_BACKEND=broadway BROADWAY_DISPLAY=$display GSK_RENDERER=cairo"
    ;;
*) echo "unknown HEADLESS_BACKEND '$backend'" >&2; exit 1 ;;
esac
trap 'kill "$compositor" 2>/dev/null || true; [ "$backend" = kwin ] && rm -f "$runtime/$socket" "$runtime/$socket.lock"' EXIT INT TERM
status=0
# shellcheck disable=SC2086
env -u DISPLAY -u XDG_SESSION_TYPE $gdk GTK_A11Y=none NO_AT_BRIDGE=1 cargo test "$@" >"$HEADLESS_LOG" 2>&1 || status=$?
exit "$status"
