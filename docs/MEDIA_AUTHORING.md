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
- The helper is `monogram-media-author`, started by `src/media_author_run.rs` inside bubblewrap:
  `--unshare-all` (no network), `--cap-drop ALL`, source mounted read-only at `/in/source`, one
  fresh `0700` output directory at `/out`, a private `/tmp`, a cleared environment. Only the
  GStreamer plugin files the picture chain needs are mounted (`coreelements`, `png`, `jpeg`,
  `jpegformat`, `webp`, `videoconvertscale`, `videocrop`, `videofilter`), and the plugin scanner
  process is disabled, so no other plugin or codec can load.
- Inside, the helper applies `RLIMIT_AS`, `RLIMIT_FSIZE` (the kernel enforces the output cap),
  `RLIMIT_NOFILE`, no core dumps, `no_new_privs`, a seccomp deny-list and an alarm. The list covers
  processes, sockets, tracing, mounts and namespaces plus the kernel attack surface escapes go
  through: io_uring, bpf, key rings, userfaultfd, perf events, module loading, the new mount API and
  kernel handles. Threads stay allowed because GStreamer needs them. `monogram-media-author
  selftest` hardens itself like a real run and tries the forbidden calls, so a test proves the
  filter refuses them (EPERM) while ordinary calls and threads still work.
  The runner also kills it after 40 seconds or when the user cancels.
- Only JPEG, PNG and WebP are accepted. The format and pixel size are read from the file header
  (`sniff_image`) and bounded *before* any decoder runs, so a tiny file claiming a huge canvas is
  refused up front. The decoder is chosen explicitly (no `decodebin`).
- The helper re-validates the plan it receives (`Plan::check`). After it exits, the main process
  accepts the result only if it is a regular, non-symlink file within the size cap whose own
  header is PNG or JPEG with exactly the planned dimensions.
- The launcher is shared with the audio helper (`src/confined.rs`) and marks every descriptor above
  stdio close-on-exec before the exec, so nothing the app holds open reaches the sandbox
  (bubblewrap does not close inherited descriptors).
- Pipelines are built element by element from the validated `Plan`, never from a string that
  contains user input. EXIF orientation is applied first, so crop coordinates are in the picture
  as the user sees it. JPEG stays JPEG (quality 90); PNG and WebP become PNG, keeping transparency.
- Tests: header and plan logic run everywhere. `cargo test -- --ignored media_author_run` runs the
  real helper in bubblewrap (needs `cargo build --bins` first), including exact pixel checks for
  crop and rotation and hostile files (text, GIF, 60000x60000 header, truncated, empty).

## Order of operations

Crop in source coordinates, rotate, scale down to fit the output edge (never up). Video
geometry is rounded to even values. Trims apply to the source timeline and must keep at least
100 ms.

## Steps

1. Plan model, validation and limits (done, unit tested).
2. Sandboxed GStreamer helper: probe and image crop/rotate/scale (done, tested in the real
   sandbox).
3. Native editor dialog for crop and rotate (done): an Edit button on each JPEG, PNG or WebP card
   in the composer opens **Edit Picture** (`adw::Dialog`). `picture_editor.rs` holds the geometry
   (crop in view pixels, rotation, aspect locks, handle dragging, clamping, source-pixel `Job`),
   tested exhaustively without a display; `crop_area.rs` draws it and handles input;
   `picture_editor_ui.rs` is the dialog. The preview is decoded by the glycin sandbox, the edit by
   the confined helper, and the dialog refuses to open if the two disagree on the picture's
   orientation. Controls: rotate left and right, crop shape (Free, Original, Square, 4:3, 16:9),
   Reset, and a "Precise Crop" group of numeric rows. Keyboard: arrow keys move the crop, Shift
   with arrows resizes it; the numeric rows give a pointer-free, screen-reader-friendly path. Done
   writes a new file under the cache directory (`monogram-desktop/edited`, private, pruned after 24
   hours and removed with its attachment) that replaces the attachment in the same position; the
   original is never modified. Cancel, Escape or closing stops a running edit.
4. Video trim and compression with progress and cancel off the main thread.
5. Albums: multi-select, ordering, shared caption.
6. Camera capture through the desktop camera portal.
