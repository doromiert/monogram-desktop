#!/usr/bin/env bash
# Run under a disposable display (e.g. xvfb-run) with python3/PyNaCl and gnome-keyring.
set -euo pipefail
fixture_binary=$(realpath "${1:?Rust unit-test binary required}")
fixture_source=$(realpath "$(dirname "$0")/keepassxc_proxy.py")
fixture_root=$(mktemp -d /tmp/monogram-kpxc.XXXXXX)
trap 'rm -rf "$fixture_root"' EXIT
mkdir "$fixture_root/bin"
cat > "$fixture_root/bin/keepassxc-proxy" <<SH
#!/usr/bin/env bash
exec python3 $(printf '%q' "$fixture_source")
SH
chmod +x "$fixture_root/bin/keepassxc-proxy"
export PATH="$fixture_root/bin:$PATH"
for fixture_case in keepassxc::native_test::protocol_fixture_roundtrip keepassxc_ui::tests::native_fill_feedback; do
    export MONOGRAM_KEEPASS_FIXTURE_STATE="$fixture_root/$fixture_case.json"
    TMPDIR=/tmp bash "$(dirname "$0")/keyring_fixture.sh" "$fixture_binary" "${2:-/etc/dbus-1/session.conf}" "$fixture_case"
done

MONOGRAM_KEEPASS_FIXTURE_TAMPER=1 TMPDIR=/tmp bash "$(dirname "$0")/keyring_fixture.sh" "$fixture_binary" "${2:-/etc/dbus-1/session.conf}" keepassxc::rejection_tests::rejects_unverified_response
MONOGRAM_KEEPASS_FIXTURE_SLOW=1 TMPDIR=/tmp bash "$(dirname "$0")/keyring_fixture.sh" "$fixture_binary" "${2:-/etc/dbus-1/session.conf}" keepassxc::rejection_tests::cancels_pending_proxy_without_blocking
