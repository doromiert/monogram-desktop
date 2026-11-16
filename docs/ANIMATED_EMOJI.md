# Animated emoji

A message whose entire text is **exactly one emoji** that is bundled plays its Noto animation: once
when it scrolls into view, and again when clicked. It rests on the first frame, which looks like the
normal emoji. Two or more emoji, any other text, links, formatting, custom emoji, or an emoji that
is not in the pack all keep the ordinary static rendering.

It only applies while the "big emoji" setting is on, and never when GNOME's animations are turned
off (`gtk-enable-animations`): then nothing is decoded and the ordinary emoji stays.

## Where the data comes from

- `assets/noto-animated-emoji.pack` is the curated set: the **250 most popular** Noto animations
  (about 2.1 MB), committed and shipped with the app. It works offline and sends nothing anywhere.
- `tools/pack-noto-emoji.py` builds it from Google's published animations. It fetches over HTTPS from two fixed hosts only, treats the index as untrusted (code points must be plain hex before they reach a URL), caps each download, and writes `noto-animated-emoji.pack.sha256` and `noto-animated-emoji.pack.sources.tsv` (the SHA-256 of every upstream file used). A unit test ties the committed pack to both, and the build is reproducible: regenerating from upstream gave a byte-identical pack. `--count N` changes the
  size; every animation is checked against the renderer limits and skipped if outside them.
- Skin-tone and other multi-person variants are only included if they rank in the top 250.

### Full set (placeholder)

The curated pack is the default on purpose. Anyone who wants every animation can build the full set
(881 animations, about 11 MB) without changing code:

```
tools/pack-noto-emoji.py --all        # writes assets/noto-animated-emoji-full.pack
```

The app loads `noto-animated-emoji-full.pack` first when it is present next to the curated pack and
falls back to the curated one, so a distribution can ship it as an optional package, or a user can
drop it into the asset directory. It is not committed and not part of the default build.

## Pack format

Little endian. `"MGNE"`, `u32` version (1), `u32` count, then `count` entries of
`u8 key_length, key, u32 offset, u32 length`, then the blobs. The key is the lowercase hex code
points joined by `-` with the presentation selectors removed, so "❤" and "❤️" share an entry. Each
blob is a gzip-compressed Lottie file, the same shape as a `.tgs` sticker.

`src/noto_emoji.rs` treats the file as untrusted: keys must be sorted, unique and well formed,
every offset is bounds-checked, entries are capped at 1 MiB, and a bad pack is refused rather than
read past its end.

## Rendering and security

Frames are rendered by the existing bubblewrap-confined tlottie helper
(`monogram-sticker-renderer`), never in the GTK process. Emoji use a separate request type,
`MGE1`, with larger limits than stickers need: square up to 1024 px, 360 frames and 6.1 seconds.
Stickers (`MGT1`) keep their exact 512x512, 180 frame, 3 second limits. The bundled bytes reach the
helper through an anonymous in-memory file, so nothing is written to disk.

At most four animations play at once (each is a confined helper process), so a flood of
single-emoji messages cannot start a crowd of them; the rest wait their turn.

Playback follows the animation's own clock: a slow frame makes the next one skip ahead, frames do
not queue, and the sandbox process ends when the animation does or the row goes away or off screen.
The widget has a fixed 96 px size from the start, so the bubble does not reflow when the first frame
arrives.

## Licence

Noto Emoji animations are Google's and are licensed under **CC BY 4.0**. Google's announcement says
"The artwork is available under the CC BY 4.0 license" and links the legal code
([source](https://developers.googleblog.com/en/updates-to-emoji-new-characters-new-animation-new-color-customization-and-more/),
checked 2026-11-16). The animation site itself shows no licence text, so that announcement is the
statement relied on. CC BY 4.0 requires credit, a link to the licence and a note of changes; all
three are in `assets/NOTICE.md` and in About > Legal. The only change is repackaging (minified,
gzip-compressed Lottie in one pack); the artwork is untouched. The full set (`--all`) is the same
artwork under the same licence.

## Tests

- `cargo test noto_emoji`: key matching (one emoji only), pack parsing and rejection of corrupt or
  hostile packs, and a check that every bundled animation is inside the renderer limits.
- `cargo test -- --ignored lottie`: the real sandbox renders every bundled emoji and each one moves;
  emoji limits do not leak into sticker mode. Build the helper first (`cargo build --bins`).
- `cargo test -- --ignored animated_emoji`: a real GTK widget swaps in moving frames at real time
  and a click replays. It opens a small window.
