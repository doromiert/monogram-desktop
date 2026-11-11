{ pkgs, lib, stdenv, pkg-config, libomemo-c, openssl, gtk4, libadwaita, libepoxy, libglycin-gtk4, glycin-loaders, sqlite, wrapGAppsHook4, xorg-server, dbus, makeFontsConf, adwaita-fonts, gsettings-desktop-schemas, mesa, librsvg, prosody, cacert, python3, jdk21, gnome-keyring, gst_all_1, clapper-unwrapped, monogramSrc ? lib.cleanSource ./., craneLib ? import (builtins.fetchTarball {
  url = "https://github.com/ipetkov/crane/archive/79fd63528501446749c09f08cad486a58c7d173e.tar.gz";
  sha256 = "sha256-TREenflwEjxOSq292FYwbc3kEm7TDRtXCZRNWEdjBmg=";
}) { inherit pkgs; } }:
let
  src = monogramSrc;
  fontConfig = makeFontsConf { fontDirectories = [ adwaita-fonts ]; };
  commonArgs = {
    pname = "monogram-desktop";
    version = "0.2.0";
    inherit src;
    strictDeps = true;
    CARGO_BUILD_TARGET = stdenv.hostPlatform.rust.rustcTarget;
    nativeBuildInputs = [ pkg-config wrapGAppsHook4 ];
    buildInputs = [ clapper-unwrapped clapper-unwrapped.lib gtk4 libadwaita libepoxy libglycin-gtk4 glycin-loaders sqlite libomemo-c openssl librsvg gsettings-desktop-schemas gst_all_1.gstreamer gst_all_1.gst-plugins-base gst_all_1.gst-plugins-good gst_all_1.gst-plugins-bad gst_all_1.gst-plugins-ugly gst_all_1.gst-libav ];
    doCheck = false;
  };
  # Only manifests/lockfiles and the real path dependency affect this source.
  dummySrc = craneLib.mkDummySrc {
    inherit src;
    extraDummyScript = ''
      rm -rf "$out/vendor/tokio-xmpp"
      cp -R ${builtins.path { path = src + "/vendor/tokio-xmpp"; name = "monogram-tokio-xmpp"; }} "$out/vendor/tokio-xmpp"
      chmod -R u+w "$out/vendor/tokio-xmpp"
    '';
  };
  cargoVendorDir = craneLib.vendorCargoDeps { inherit src; };
  cargoArtifacts = craneLib.buildDepsOnly ((builtins.removeAttrs commonArgs [ "src" ]) // {
    inherit dummySrc cargoVendorDir;
    cargoCheckExtraArgs = "--bin monogram-desktop";
    cargoBuildExtraArgs = "--bin monogram-desktop";
  });
in craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts cargoVendorDir;
  cargoBuildExtraArgs = "--bin monogram-desktop";
  passthru = { inherit cargoArtifacts; };
  nativeCheckInputs = [ xorg-server dbus prosody openssl python3 jdk21 gnome-keyring ];
  postCheck = ''
    export HOME=$TMPDIR/home XDG_CACHE_HOME=$TMPDIR/cache XDG_CONFIG_HOME=$TMPDIR/config XDG_STATE_HOME=$TMPDIR/state
    mkdir -p "$HOME" "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
    Xvfb :87 -screen 0 2000x1200x24 -nolisten tcp >xvfb.log 2>&1 &
    xvfb_pid=$!
    trap 'kill "$xvfb_pid" 2>/dev/null || true' EXIT
    export LIBGL_ALWAYS_SOFTWARE=1
    export __EGL_VENDOR_LIBRARY_FILENAMES=${mesa}/share/glvnd/egl_vendor.d/50_mesa.json
    export LIBGL_DRIVERS_PATH=${mesa}/lib/dri
    export MONOGRAM_EPOXY_LIBRARY=${libepoxy}/lib/libepoxy.so.0
    export DISPLAY=:87 GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=none G_DEBUG=fatal-criticals
    export FONTCONFIG_FILE=${fontConfig}
    export GSETTINGS_SCHEMA_DIR=${gsettings-desktop-schemas}/share/gsettings-schemas/${gsettings-desktop-schemas.name}/glib-2.0/schemas
    export XDG_DATA_DIRS=${gtk4}/share/gsettings-schemas/${gtk4.name}:''${XDG_DATA_DIRS:-}
    export JAVA_HOME=${jdk21}/lib/openjdk
    bash tests/mobile-reference/build.sh "$TMPDIR/mobile-crypto"
    crypto_test=
    for candidate in target/${stdenv.hostPlatform.rust.rustcTarget}/release/deps/monogram_desktop-*; do
      if [ -f "$candidate" ] && [ -x "$candidate" ] && "$candidate" --list 2>/dev/null | grep -Fqx 'crypto::tests::native_mobile_jvm_interoperability: test'; then
        crypto_test=$candidate
        break
      fi
    done
    test -n "$crypto_test"
    MONOGRAM_MOBILE_CRYPTO_FIXTURE="$TMPDIR/mobile-crypto" "$crypto_test" --ignored --exact crypto::tests::native_mobile_jvm_interoperability
    bash tests/keyring_fixture.sh "$crypto_test" ${dbus}/share/dbus-1/session.conf
    bash tests/keyring_fixture.sh "$crypto_test" ${dbus}/share/dbus-1/session.conf crypto::lifecycle::tests::isolated_identity_lifecycle
    bash tests/keyring_fixture.sh "$crypto_test" ${dbus}/share/dbus-1/session.conf history_window_ui::tests::live_viewport_keeps_archive_reply_and_incoming_updates
    dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- target/${stdenv.hostPlatform.rust.rustcTarget}/release/monogram-desktop --smoke-test --show-media
    transport_test=$(find target -path '*/release/deps/transport-*' -type f -executable | head -n 1)
    dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- ${python3}/bin/python tests/xmpp_fixture.py --test-binary "$transport_test" --crypto-test-binary "$crypto_test" --bus-config ${dbus}/share/dbus-1/session.conf --app-binary target/${stdenv.hostPlatform.rust.rustcTarget}/release/monogram-desktop
    kill "$xvfb_pid"
    trap - EXIT
  '';
  postInstall = ''
    mkdir -p $out/share/monogram-desktop $out/share/applications $out/share/icons/hicolor/scalable/apps $out/share/doc/monogram-desktop
    cp -r assets/. $out/share/monogram-desktop/
    cp data/com.negzero.MonogramDesktop.desktop $out/share/applications/
    cp data/icons/com.negzero.MonogramDesktop.svg $out/share/icons/hicolor/scalable/apps/
    cp -r LICENSE LICENSES $out/share/doc/monogram-desktop/
    cp native/NOTICE.md $out/share/doc/monogram-desktop/native-NOTICE.md
    gappsWrapperArgs+=(
      --set MONOGRAM_ASSET_DIR $out/share/monogram-desktop
      --set MONOGRAM_EPOXY_LIBRARY ${libepoxy}/lib/libepoxy.so.0
      --set GTK_USE_PORTAL 1
      --set-default SSL_CERT_FILE ${cacert}/etc/ssl/certs/ca-bundle.crt
    )
  '';
  meta = {
    description = "Native Rust GTK/libadwaita Monogram client";
    mainProgram = "monogram-desktop";
    license = [ lib.licenses.gpl3Plus lib.licenses.gpl3Only ];
    platforms = lib.platforms.linux;
  };
})
