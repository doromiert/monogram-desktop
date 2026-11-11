# Native components

`crypto_provider.c` is copied unchanged from Monogram Android
`xmpp-core/src/main/c/crypto_provider.c` at commit
`be6876c045163e32fe07a84416621770b0964b30`, GPL-3.0-or-later.
SHA-256: `8c5c7daf2dff99051e20d96adddd48fe6e3a3cf2245a05829a9fddfab6155dcf`.

`omemo.cpp` adapts that client's `omemo2_jni.cpp` to a small C ABI for Rust.
It replaces JNI/Java record callbacks with Rust transaction callbacks and removes
JVM-specific conversion code. The protocol and ratchet remain upstream libomemo-c.
Adapted 2026-11-10; GPL-3.0-or-later.

The linked [Dino libomemo-c](https://github.com/dino/libomemo-c) 0.5.1 package is
the same source as mobile's pinned commit
`1f0b37fee3ebefc46c32f882623481899a1ba568`.
Source NAR hash: `sha256-HoZykdGVDsj4L5yN3SHGF5tjMq5exJyC15zTLBlpX/c=`.
Its upstream GPL-3.0-only license and retained third-party notices apply. The library
is provided through the pinned Nix package, rather than copied into this tree.
OpenSSL supplies the existing mobile-compatible cryptographic provider.

The adapter/provider are GPL-3.0-or-later; the combined build uses GPL-3.0. See
`LICENSES/GPL-3.0-or-later.txt`. The Java/JNI comparison fixtures are test-only;
see `tests/mobile-reference/README.md` for their provenance.

`image_loader.c` is a thin adapter to [GNOME Glycin](https://gnome.pages.gitlab.gnome.org/glycin/)
2.1.5 from Nix. It hands verified descriptors to the upstream sandbox and returns
decoded textures to Rust.

`video_player.c` links [Clapper and Clapper GTK](https://github.com/Rafostar/clapper)
0.10.0 through the pinned Nix package (LGPL-2.1-or-later, copyright Rafał Dzięgiel).
It reuses `ClapperGtk.Video` and `SimpleControls`, the same upstream widgets used
by Galeria's `video_ui.py`, while adapting their paintable for Monogram's shared
inline/focus rendering. No Clapper implementation is vendored here.

`lottie_renderer.c` calls the upstream [Samsung rlottie C API](https://github.com/Samsung/rlottie), linked from Nix rlottie 0.2-unstable-2026-10-01. rlottie is MIT with its retained upstream third-party notices; no library source is copied. The GPL-3.0-or-later helper parses TGS only inside bubblewrap and renders one requested RGBA frame at a time.
