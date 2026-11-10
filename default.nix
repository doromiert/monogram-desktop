{ lib, stdenv, rustPlatform, pkg-config, gtk4, libadwaita, libepoxy, sqlite, wrapGAppsHook4, xorg-server, dbus, makeFontsConf, adwaita-fonts, gsettings-desktop-schemas, mesa, librsvg, prosody, openssl, cacert, python3 }:
let fontConfig = makeFontsConf { fontDirectories = [ adwaita-fonts ]; };
in rustPlatform.buildRustPackage {
  pname = "monogram-desktop";
  version = "0.2.0";
  src = lib.cleanSource ./.;
  cargoHash = "sha256-17o4mtrMjdoEmfAeMfqEz0Q8QUpuD48LNRenAmB2MjI=";
  nativeBuildInputs = [ pkg-config wrapGAppsHook4 ];
  buildInputs = [ gtk4 libadwaita libepoxy sqlite librsvg gsettings-desktop-schemas ];
  nativeCheckInputs = [ xorg-server dbus prosody openssl python3 ];
  doCheck = true;
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
    dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- target/${stdenv.hostPlatform.rust.rustcTarget}/release/monogram-desktop --smoke-test --show-media
    transport_test=$(find target -path '*/release/deps/transport-*' -type f -executable | head -n 1)
    dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- ${python3}/bin/python tests/xmpp_fixture.py --test-binary "$transport_test" --app-binary target/${stdenv.hostPlatform.rust.rustcTarget}/release/monogram-desktop
    kill "$xvfb_pid"
    trap - EXIT
  '';
  postInstall = ''
    mkdir -p $out/share/monogram-desktop $out/share/applications $out/share/icons/hicolor/scalable/apps $out/share/doc/monogram-desktop
    cp -r assets/. $out/share/monogram-desktop/
    cp data/com.negzero.MonogramDesktop.desktop $out/share/applications/
    cp data/icons/com.negzero.MonogramDesktop.svg $out/share/icons/hicolor/scalable/apps/
    cp -r LICENSE LICENSES $out/share/doc/monogram-desktop/
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
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
  };
}
