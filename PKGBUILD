# Maintainer: doromiert <https://github.com/doromiert>
# Keep makepkg --cleanbuild from removing this checkout's Rust src/ directory.
if [[ -f "$startdir/Cargo.toml" && "$BUILDDIR" -ef "$startdir" ]]; then
    BUILDDIR="$startdir/.state/makepkg"
    if [[ "$SRCDEST" -ef "$startdir" ]]; then
        SRCDEST="$BUILDDIR/sources"
        mkdir -p "$SRCDEST"
    fi
fi
pkgname=monogram-desktop-git
pkgver=0.2.0.r135.g6594be3
pkgrel=1
pkgdesc='Native GTK/libadwaita Monogram XMPP client'
arch=('x86_64' 'aarch64')
url='https://github.com/doromiert/monogram-desktop'
license=('GPL-3.0-or-later' 'GPL-3.0-only' 'AGPL-3.0-only' 'LGPL-2.1-or-later' 'NAPL-1.1')
depends=('gtk4>=1:4.20' 'libadwaita>=1:1.9' 'glycin-gtk4>=2.1' 'glycin'
         'libclapper-gtk>=0.10' 'gst-plugins-base' 'gst-plugins-good' 'gst-plugins-bad'
         'gst-plugins-ugly' 'gst-libav' 'libepoxy' 'openssl' 'sqlite' 'protobuf-c'
         'bubblewrap' 'fontconfig' 'ttf-dejavu' 'noto-fonts-emoji' 'jre21-openjdk-headless')
makedepends=('git' 'rust' 'cargo' 'pkgconf' 'protobuf' 'cmake' 'ninja' 'jdk21-openjdk' 'glib2-devel')
optdepends=('keepassxc: fill and save XMPP passwords' 'gnome-keyring: Secret Service provider (or another compatible keyring)')
provides=('monogram-desktop')
conflicts=('monogram-desktop')
options=('!lto')
_omemo_rev=1f0b37fee3ebefc46c32f882623481899a1ba568
_rlottie_rev=ea06d2f29ba01b8d06c00a838d107f5e484ae59b
source=("monogram-desktop::git+$url.git"
        "libomemo-c-$_omemo_rev.tar.gz::https://codeload.github.com/dino/libomemo-c/tar.gz/$_omemo_rev"
        "rlottie-$_rlottie_rev.tar.gz::https://codeload.github.com/Samsung/rlottie/tar.gz/$_rlottie_rev"
        'https://repo.maven.apache.org/maven2/org/scilab/forge/jlatexmath/1.0.7/jlatexmath-1.0.7.jar'
        'https://repo.maven.apache.org/maven2/org/scilab/forge/jlatexmath-font-greek/1.0.7/jlatexmath-font-greek-1.0.7.jar'
        'https://repo.maven.apache.org/maven2/org/scilab/forge/jlatexmath-font-cyrillic/1.0.7/jlatexmath-font-cyrillic-1.0.7.jar')
noextract=('jlatexmath-1.0.7.jar' 'jlatexmath-font-greek-1.0.7.jar' 'jlatexmath-font-cyrillic-1.0.7.jar')
sha256sums=('SKIP'
            'f5a2b8751b3ad2a40a367e4ed45a671338b5d3ed09e36d9f86e06fd96d7da11b'
            'a89e071a3455ccbf8f61af67aad0657259ae9aabdf3afe3a0670954370cdd37a'
            '6d04c3843fc98d2eea9ba4cff0be7b80540ca75c5a4e0b3463fd1219c48cc65d'
            'cddc7e7e6f3e7ff0434c8a4805eadc3e9dc80457d582d19c3699e6aaee8f6af2'
            '1a0150e4be64db88c250dff41f593349f55f0c9cfdb8e39433f15ba73ecd21b8')

pkgver() {
    cd "$srcdir/monogram-desktop"
    printf '0.2.0.r%s.g%s' "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

prepare() {
    cd "$srcdir/monogram-desktop"
    cargo fetch --locked
}

build() {
    # Match mobile/Nix's OMEMO backend without installing a second system provider.
    cmake -S "$srcdir/libomemo-c-$_omemo_rev" -B "$srcdir/omemo-build" -G Ninja \
        -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$srcdir/native" -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
        -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF
    cmake --build "$srcdir/omemo-build"
    cmake --install "$srcdir/omemo-build"
    cmake -S "$srcdir/rlottie-$_rlottie_rev" -B "$srcdir/rlottie-build" -G Ninja \
        -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$srcdir/native" -DLIB_INSTALL_DIR="$srcdir/native/lib" -DBUILD_SHARED_LIBS=OFF \
        -DLOTTIE_THREAD=OFF -DLOTTIE_MODULE=OFF -DLOTTIE_TEST=OFF
    cmake --build "$srcdir/rlottie-build"
    cmake --install "$srcdir/rlottie-build"
    cd "$srcdir/monogram-desktop"
    cc $CFLAGS -std=c11 -I"$srcdir/native/include" -c native/lottie_renderer.c -o "$srcdir/lottie-renderer.o"
    c++ $LDFLAGS "$srcdir/lottie-renderer.o" "$srcdir/native/lib/librlottie.a" -lz -lm -pthread -o "$srcdir/monogram-sticker-renderer"
    mkdir -p "$srcdir/math"
    /usr/lib/jvm/java-21-openjdk/bin/javac -cp "$srcdir/jlatexmath-1.0.7.jar" -d "$srcdir/math" native/MonogramMath.java
    export PKG_CONFIG_PATH="$srcdir/native/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    export MONOGRAM_PREBUILT_LOTTIE_HELPER="$srcdir/monogram-sticker-renderer"
    export MONOGRAM_BWRAP=/usr/bin/bwrap MONOGRAM_MATH_HELPER=/usr/lib/monogram-desktop/monogram-math-renderer
    cargo build --release --frozen
}

check() {
    cd "$srcdir/monogram-desktop"
    export PKG_CONFIG_PATH="$srcdir/native/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    export MONOGRAM_PREBUILT_LOTTIE_HELPER="$srcdir/monogram-sticker-renderer"
    export MONOGRAM_BWRAP=/usr/bin/bwrap MONOGRAM_MATH_HELPER=/usr/lib/monogram-desktop/monogram-math-renderer
    cargo test --release --frozen --bin monogram-desktop keepassxc::tests
}

package() {
    cd "$srcdir/monogram-desktop"
    install -Dm755 target/release/monogram-desktop "$pkgdir/usr/lib/monogram-desktop/monogram-desktop"
    install -Dm755 "$srcdir/monogram-sticker-renderer" "$pkgdir/usr/lib/monogram-desktop/monogram-sticker-renderer"
    install -Dm644 "$srcdir/math/MonogramMath.class" "$pkgdir/usr/lib/monogram-desktop/math/MonogramMath.class"
    install -m644 "$srcdir/"jlatexmath*.jar "$pkgdir/usr/lib/monogram-desktop/math/"
    install -Dm755 packaging/monogram-desktop "$pkgdir/usr/bin/monogram-desktop"
    install -Dm755 packaging/monogram-math-renderer "$pkgdir/usr/lib/monogram-desktop/monogram-math-renderer"
    install -d "$pkgdir/usr/share/monogram-desktop" "$pkgdir/usr/share/licenses/$pkgname"
    cp -a assets/. "$pkgdir/usr/share/monogram-desktop/"
    install -Dm644 data/com.negzero.MonogramDesktop.desktop "$pkgdir/usr/share/applications/com.negzero.MonogramDesktop.desktop"
    install -Dm644 data/icons/com.negzero.MonogramDesktop.svg "$pkgdir/usr/share/icons/hicolor/scalable/apps/com.negzero.MonogramDesktop.svg"
    cp -a LICENSE LICENSES "$pkgdir/usr/share/licenses/$pkgname/"
    install -m644 native/NOTICE.md "$pkgdir/usr/share/licenses/$pkgname/native-NOTICE.md"
    install -m644 "$srcdir/libomemo-c-$_omemo_rev/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/libomemo-c-LICENSE"
    install -m644 "$srcdir/rlottie-$_rlottie_rev/COPYING" "$pkgdir/usr/share/licenses/$pkgname/rlottie-COPYING"
}
