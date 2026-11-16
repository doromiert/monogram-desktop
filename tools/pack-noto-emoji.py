#!/usr/bin/env python3
"""Build the bundled Noto animated emoji pack (assets/noto-animated-emoji.pack).

    tools/pack-noto-emoji.py                 # the curated set: the 250 most popular emoji
    tools/pack-noto-emoji.py --count 400     # a different size
    tools/pack-noto-emoji.py --all           # FULL SET placeholder, see below

The animations are Google's Noto Emoji animations (see assets/NOTICE.md for the licence and
attribution). Run this when you want to change the shipped set; the result is committed.

FULL SET: `--all` writes assets/noto-animated-emoji-full.pack (about 11 MB) instead. The app
prefers that file over the curated pack when it is present, so a distribution or a user who wants
every animation can generate it and install it next to the curated pack without touching code.
It is intentionally not committed or shipped by default.

Pack format (little endian), read by src/noto_emoji.rs:
    "MGNE" | u32 version (1) | u32 count
    count x ( u8 key_len | key | u32 offset | u32 length )   key = lowercase hex code points
                                                               joined by "-", FE0F/FE0E removed
    blobs: each entry is a gzip-compressed Lottie JSON (the same shape as a .tgs sticker)
"""
import argparse, gzip, hashlib, json, re, struct, sys, urllib.request, concurrent.futures as futures
from pathlib import Path

INDEX = "https://googlefonts.github.io/noto-emoji-animation/data/api.json"
ASSET = "https://fonts.gstatic.com/s/e/notoemoji/latest/{}/lottie.json"
# Limits the sandboxed renderer enforces for emoji (src/bin/monogram-sticker-renderer.rs).
MAX_EDGE, MAX_FRAMES, MAX_FPS, MAX_SECONDS = 1024, 360, 60, 6.1
MAX_BLOB = 1024 * 1024

# Everything is fetched over HTTPS from these two hosts only, and the index is untrusted input:
# a code point that is not plain hex would otherwise be pasted into a URL path.
ALLOWED_HOSTS = ("https://googlefonts.github.io/", "https://fonts.gstatic.com/")
CODEPOINT = re.compile(r"[0-9a-f]+(_[0-9a-f]+)*")
MAX_DOWNLOAD = 4 * 1024 * 1024

def fetch(url):
    if not url.startswith(ALLOWED_HOSTS):
        raise ValueError(f"refusing to fetch {url}")
    with urllib.request.urlopen(url, timeout=30) as reply:
        data = reply.read(MAX_DOWNLOAD + 1)
    if len(data) > MAX_DOWNLOAD:
        raise ValueError(f"{url} is larger than {MAX_DOWNLOAD} bytes")
    return data

def key_of(codepoint):
    parts = [p for p in codepoint.split("_") if p not in ("fe0f", "fe0e")]
    return "-".join(parts)

def build(entry):
    if not CODEPOINT.fullmatch(entry["codepoint"]):
        raise ValueError(f"unexpected code point {entry['codepoint']!r}")
    raw = fetch(ASSET.format(entry["codepoint"]))
    animation = json.loads(raw)
    width, height, rate = animation["w"], animation["h"], animation["fr"]
    frames = animation["op"] - animation["ip"]
    if (width != height or not 0 < width <= MAX_EDGE or not 0 < rate <= MAX_FPS
            or not 0 < frames <= MAX_FRAMES or frames / rate > MAX_SECONDS):
        raise ValueError(f"outside renderer limits: {width}x{height} {rate}fps {frames} frames")
    blob = gzip.compress(json.dumps(animation, separators=(",", ":")).encode(), 9, mtime=0)
    if len(blob) > MAX_BLOB:
        raise ValueError(f"{len(blob)} bytes compressed")
    return key_of(entry["codepoint"]), blob, hashlib.sha256(raw).hexdigest()

def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument("--count", type=int, default=250, help="most popular N emoji (default 250)")
    parser.add_argument("--all", action="store_true", help="every emoji, written as the optional full pack")
    parser.add_argument("--out", type=Path, help="output file")
    options = parser.parse_args()
    icons = sorted(json.loads(fetch(INDEX))["icons"], key=lambda i: -i["popularity"])
    chosen = icons if options.all else icons[: options.count]
    out = options.out or Path("assets") / ("noto-animated-emoji-full.pack" if options.all else "noto-animated-emoji.pack")
    entries, upstream, skipped = {}, {}, []
    with futures.ThreadPoolExecutor(8) as pool:
        for icon, result in zip(chosen, pool.map(lambda i: _safe(build, i), chosen)):
            if isinstance(result, Exception):
                skipped.append((icon["codepoint"], str(result)))
            else:
                entries[result[0]] = result[1]
                upstream[result[0]] = result[2]
    keys = sorted(entries)
    header = 12 + sum(1 + len(k) + 8 for k in keys)
    index, blobs, offset = b"", b"", header
    for key in keys:
        index += struct.pack("<B", len(key)) + key.encode() + struct.pack("<II", offset, len(entries[key]))
        blobs += entries[key]
        offset += len(entries[key])
    pack = b"MGNE" + struct.pack("<II", 1, len(keys)) + index + blobs
    out.write_bytes(pack)
    # A digest of the pack (checked by a unit test) and the SHA-256 of every upstream file it was
    # built from, so a regeneration can be compared against what Google serves.
    out.with_name(out.name + ".sha256").write_text(f"{hashlib.sha256(pack).hexdigest()}  {out.name}\n")
    out.with_name(out.name + ".sources.tsv").write_text("".join(f"{key}\t{upstream[key]}\n" for key in keys))
    print(f"{out}: {len(keys)} emoji, {out.stat().st_size / 1e6:.2f} MB, sha256 {hashlib.sha256(pack).hexdigest()}")
    for codepoint, why in skipped:
        print(f"skipped {codepoint}: {why}", file=sys.stderr)

def _safe(function, argument):
    try:
        return function(argument)
    except Exception as error:  # a missing or unusable animation is skipped, not fatal
        return error

if __name__ == "__main__":
    main()
