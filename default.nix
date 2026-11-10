{ lib, stdenv, python3, gtk4, libadwaita, wrapGAppsHook4, gobject-introspection, makeWrapper, xorg-server, dbus, makeFontsConf, adwaita-fonts, gsettings-desktop-schemas, mesa, librsvg }:
let fontConfig = makeFontsConf { fontDirectories = [ adwaita-fonts ]; };
    python = python3.withPackages (ps: [ ps.pygobject3 ps.pycairo ps.pyopengl ps.numpy ps.qrcode ]);
in stdenv.mkDerivation {
  pname = "monogram-desktop";
  version = "0.1.0-prototype";
  src = lib.cleanSource ./.;
  nativeBuildInputs = [ wrapGAppsHook4 gobject-introspection makeWrapper ];
  buildInputs = [ gtk4 libadwaita librsvg gsettings-desktop-schemas ];
  nativeCheckInputs = [ xorg-server dbus ];
  dontBuild = true;
  doCheck = true;
  checkPhase = ''
    runHook preCheck
    export PYTHONPATH=$PWD/src
    export HOME=$TMPDIR/home XDG_CACHE_HOME=$TMPDIR/cache XDG_CONFIG_HOME=$TMPDIR/config
    mkdir -p "$HOME" "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME"
    ${python}/bin/python -m unittest discover -s tests -v
    Xvfb :87 -screen 0 1440x1000x24 -nolisten tcp >xvfb.log 2>&1 &
    xvfb_pid=$!
    trap 'kill "$xvfb_pid" 2>/dev/null || true' EXIT
    export PYOPENGL_PLATFORM=egl LIBGL_ALWAYS_SOFTWARE=1
    export __EGL_VENDOR_LIBRARY_FILENAMES=${mesa}/share/glvnd/egl_vendor.d/50_mesa.json
    export LIBGL_DRIVERS_PATH=${mesa}/lib/dri
    export DISPLAY=:87 GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=none G_DEBUG=fatal-criticals
    export FONTCONFIG_FILE=${fontConfig}
    export GSETTINGS_SCHEMA_DIR=${gsettings-desktop-schemas}/share/gsettings-schemas/${gsettings-desktop-schemas.name}/glib-2.0/schemas
    dbus-run-session --config-file=${dbus}/share/dbus-1/session.conf -- ${python}/bin/python -m monogram_desktop --smoke-test
    kill "$xvfb_pid"
    trap - EXIT
    runHook postCheck
  '';
  installPhase = ''
    mkdir -p $out/lib/monogram-desktop $out/bin $out/share/applications $out/share/icons/hicolor/scalable/apps $out/share/doc/monogram-desktop
    cp -r src/monogram_desktop $out/lib/monogram-desktop/
    cp data/com.negzero.MonogramDesktop.desktop $out/share/applications/
    cp data/icons/com.negzero.MonogramDesktop.svg $out/share/icons/hicolor/scalable/apps/
    cp -r LICENSE LICENSES $out/share/doc/monogram-desktop/
    makeWrapper ${python}/bin/python $out/bin/monogram-desktop \
      --add-flags "-m monogram_desktop" \
      --prefix PYTHONPATH : $out/lib/monogram-desktop \
      --set GTK_USE_PORTAL 1 \
      --set PYOPENGL_PLATFORM egl
  '';
  meta = {
    description = "Backend-free native Monogram Desktop prototype";
    mainProgram = "monogram-desktop";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
  };
}
