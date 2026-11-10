#!/usr/bin/env bash
set -euo pipefail
: "${JAVA_HOME:?JDK 21 is required}"
fixture_out=${1:?Fixture build directory required}
mkdir -p "$fixture_out/classes"
# Only source/native code is compiled; no accounts or real key stores are opened.
${CC:-cc} -O2 -fPIC -Wno-deprecated-declarations $(pkg-config --cflags libomemo-c openssl) -c native/crypto_provider.c -o "$fixture_out/provider.o"
${CXX:-c++} -std=c++17 -O2 -fPIC -shared -Wno-deprecated-declarations \
  -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/linux" $(pkg-config --cflags libomemo-c openssl) \
  tests/mobile-reference/omemo2_jni.cpp "$fixture_out/provider.o" \
  $(pkg-config --libs libomemo-c openssl) \
  -Wl,-rpath,"$(pkg-config --variable=libdir libomemo-c)" \
  -o "$fixture_out/libmonogram_omemo2.so"
"$JAVA_HOME/bin/javac" -d "$fixture_out/classes" tests/mobile-reference/src/com/negzero/monogram/xmpp/*.java
