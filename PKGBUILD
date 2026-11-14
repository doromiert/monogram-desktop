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
license=('GPL-3.0-or-later' 'GPL-3.0-only' 'AGPL-3.0-only' 'LGPL-2.1-or-later' 'NAPL-1.1' 'OFL-1.1')
depends=('gtk4>=1:4.20' 'libadwaita>=1:1.9' 'glycin-gtk4>=2.1' 'glycin'
         'libclapper-gtk>=0.10' 'gst-plugins-base' 'gst-plugins-good' 'gst-plugins-bad'
         'gst-plugins-ugly' 'gst-libav' 'libepoxy' 'openssl' 'sqlite' 'protobuf-c'
         'bubblewrap' 'fontconfig' 'ttf-dejavu' 'noto-fonts-emoji')
makedepends=('git' 'rust' 'cargo' 'pkgconf' 'protobuf' 'cmake' 'ninja' 'glib2-devel')
optdepends=('keepassxc: fill and save XMPP passwords' 'gnome-keyring: Secret Service provider (or another compatible keyring)')
provides=('monogram-desktop')
conflicts=('monogram-desktop')
options=('!lto')
_omemo_rev=1f0b37fee3ebefc46c32f882623481899a1ba568
source=("monogram-desktop::git+$url.git"
        "libomemo-c-$_omemo_rev.tar.gz::https://codeload.github.com/dino/libomemo-c/tar.gz/$_omemo_rev")
sha256sums=('SKIP'
            'f5a2b8751b3ad2a40a367e4ed45a671338b5d3ed09e36d9f86e06fd96d7da11b')

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
    cd "$srcdir/monogram-desktop"
    export PKG_CONFIG_PATH="$srcdir/native/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    export MONOGRAM_BWRAP=/usr/bin/bwrap
    cargo build --release --frozen --bins
}

check() {
    cd "$srcdir/monogram-desktop"
    export PKG_CONFIG_PATH="$srcdir/native/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    export MONOGRAM_BWRAP=/usr/bin/bwrap
    cargo test --release --frozen --bin monogram-desktop keepassxc::tests
}

package() {
    cd "$srcdir/monogram-desktop"
    install -Dm755 target/release/monogram-desktop "$pkgdir/usr/lib/monogram-desktop/monogram-desktop"
    install -Dm755 target/release/monogram-sticker-renderer "$pkgdir/usr/lib/monogram-desktop/monogram-sticker-renderer"
    install -Dm755 target/release/monogram-math-renderer "$pkgdir/usr/lib/monogram-desktop/monogram-math-renderer"
    install -Dm755 packaging/monogram-desktop "$pkgdir/usr/bin/monogram-desktop"
    install -d "$pkgdir/usr/share/monogram-desktop" "$pkgdir/usr/share/licenses/$pkgname"
    cp -a assets/. "$pkgdir/usr/share/monogram-desktop/"
    install -Dm644 data/com.negzero.MonogramDesktop.desktop "$pkgdir/usr/share/applications/com.negzero.MonogramDesktop.desktop"
    install -Dm644 data/icons/com.negzero.MonogramDesktop.svg "$pkgdir/usr/share/icons/hicolor/scalable/apps/com.negzero.MonogramDesktop.svg"
    cp -a LICENSE LICENSES "$pkgdir/usr/share/licenses/$pkgname/"
    install -m644 native/NOTICE.md "$pkgdir/usr/share/licenses/$pkgname/native-NOTICE.md"
    install -m644 "$srcdir/libomemo-c-$_omemo_rev/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/libomemo-c-LICENSE"
}
