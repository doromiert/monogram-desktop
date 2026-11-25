# Generated HDR fixtures

These are synthetic patterns, not user photos or messages. `gainmap.jpg` is a
64×32 BT.2020/PQ horizontal gradient generated with Google's libultrahdr1.4 demo
encoder (`-m0 -a5 -t2 -C2 -s1 -D0`). Each packed RGBA1010102 pixel has R=G=B=
600+5*x and alpha3. `gainmap-90.jpg` uses the same HDR input plus the existing
64×32 SMPTE SDR fixture with a standard TIFF orientation6 EXIF block, so the
encoder updates the MPF offsets correctly. The helper must display it as32×64.

`pq.mp4` and `hlg.mp4` are one-second160×90,15fps FFmpeg testsrc2 patterns encoded
with libx265, yuv420p10le, BT.2020 primaries/matrix and respectively smpte2084 and
arib-std-b67 transfer characteristics. Encoder threads/pools are bounded to1.

The source encoder/decoder implementation is not vendored. libultrahdr is
Apache-2.0 licensed; see https://github.com/google/libultrahdr.
