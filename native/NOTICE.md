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

The adapter/provider are GPL-3.0-or-later. See
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

## Monocles PQ OMEMO

The independent `src/crypto/pq/` provider adapts Monogram mobile's Monocles
PQ-OMEMO profile adapters and the Monocles wire profile. These adaptations and
the pinned libsignal fork are licensed under AGPL-3.0-only.

- Monocles libsignal fork: <https://codeberg.org/monocles/pq-omemo-2>, revision
  `10861bff671740174f4e99f7e30dc492fcf6ec8b`. Upstream copyright Signal Messenger,
  LLC and its contributors; Monocles PQ identity, PQXDH and SPQR adaptations
  retain their upstream notices.
- Mobile adapter contract: `monogram/xmpp-core` classes `PqOmemoBundle`,
  `PqOmemoMessage`, `PqOmemoPayload`, `PqOmemoPep`, and `XmppPqOmemo`; Monocles
  protocol/codec reference revision `49c0e78045532a78033ffd448ca4316859267bf2`.
- The AGPL text is included in `LICENSES/AGPL-3.0-only.txt`. Existing components
  retain their own licenses and attribution.

## GTK GPU clip-state correction

Monogram's Nix package uses GTK 4.22.4 with the one-line correction in
`native/gtk-gpu-clip-restore.patch`, preserving the previous clip when an
opaque-region candidate cannot be used. This dependency is private to the
application; system GTK is unchanged. Upstream source and attribution:
<https://github.com/GNOME/gtk/blob/4.22.4/gsk/gpu/gskgpunodeprocessor.c>.
GTK is LGPL-2.1-or-later; the original source retains its copyright notices.
The patch is installed beside this notice in the application documentation.
