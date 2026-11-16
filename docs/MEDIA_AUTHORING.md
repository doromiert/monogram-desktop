# Media authoring

Crop, rotate, trim and compress photos and videos before sending. GStreamer is the backend
because it is packaged on every mainstream distribution and already powers video playback.

## Security model

Authoring reads files the user may have downloaded, so it is treated like any other untrusted
media: it never runs in the main process.

- `src/media_authoring.rs` only does arithmetic. A `Job` (what the editor asks for) plus the
  `SourceInfo` the helper found in the file become a `Plan`, or an `Error`. Nothing is clamped
  silently; out-of-range requests are refused.
- Hard limits (`Limits`): 512 MiB input, 64 megapixels and 16384 px per edge, 1 hour of video,
  4096 px output edge, 256 MiB output, 16 KiB job description. Job and plan JSON reject unknown
  fields.
- The helper (next step) is a bubblewrap-confined process like the math and sticker helpers:
  `--unshare-all`, read-only input, one writable output directory, no network, and it deletes its
  output if it passes `max_output_bytes`. Probe results come back as `SourceInfo` and are
  validated again by the main process.
- Pipelines are built element by element from the validated `Plan`, never from a string that
  contains user input.

## Order of operations

Crop in source coordinates, rotate, scale down to fit the output edge (never up). Video
geometry is rounded to even values. Trims apply to the source timeline and must keep at least
100 ms.

## Steps

1. Plan model, validation and limits (done, unit tested).
2. Sandboxed GStreamer helper: probe and image crop/rotate/scale.
3. Native editor dialog (`adw::Dialog`, keyboard operable, accessible names) for crop and rotate.
4. Video trim and compression with progress and cancel off the main thread.
5. Albums: multi-select, ordering, shared caption.
6. Camera capture through the desktop camera portal.
