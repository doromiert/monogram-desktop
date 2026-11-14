{ stdenv, lib, fetchFromGitHub, cmake, pkg-config, pangomm_2_48, cairomm_1_16, tinyxml-2, fontconfig, freetype }:
let source = fetchFromGitHub {
  owner = "NanoMichael";
  repo = "MicroTeX";
  rev = "0e3707f6dafebb121d98b53c64364d16fefe481d";
  hash = "sha256-U6zqh+VqoLtlE0IwgfwjY9zt8e5/2R3cqf5fWXwoIi0=";
}; in stdenv.mkDerivation {
  pname = "monogram-math-renderer";
  version = "0.2.0";
  src = ./.;
  nativeBuildInputs = [ cmake pkg-config ];
  buildInputs = [ pangomm_2_48 cairomm_1_16 tinyxml-2 fontconfig freetype ];
  cmakeFlags = [
    "-DMICROTEX_SOURCE_DIR=${source}"
    "-DMONOGRAM_MATH_RESOURCE_DIR=${builtins.placeholder "out"}/share/monogram-math"
  ];
  meta.license = [ lib.licenses.gpl3Plus lib.licenses.mit ];
}
