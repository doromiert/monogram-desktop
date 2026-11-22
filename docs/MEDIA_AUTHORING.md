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
  4096 px output edge, 256 MiB output, 32 KiB job description. Job and plan JSON reject unknown
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

Draw marks (source coordinates, upright picture), then crop in source coordinates, rotate, scale
down to fit the output edge (never up). Video
geometry is rounded to even values. Trims apply to the source timeline and must keep at least
100 ms.

## Steps

1. Plan model, validation and limits (done, unit tested).
2. Sandboxed GStreamer helper: probe and image crop/rotate/scale (done, tested in the real
   sandbox).
3. Native editor dialog for crop, rotate and marks (done): an Edit button on each JPEG, PNG or WebP
   card in the composer opens **Edit Picture** (`adw::Dialog`). `picture_editor.rs` holds the
   geometry (crop in view pixels, rotation, aspect locks, handle dragging, clamping, source-pixel
   `Job`), tested exhaustively without a display; `crop_area.rs` draws it and handles input;
   `picture_editor_ui.rs` is the dialog. The preview is decoded by the glycin sandbox, the edit by
   the confined helper, and the dialog refuses to open if the two disagree on the picture's
   orientation.

   **Helper version.** `cargo run` rebuilds only the app, so `monogram-media-author` next to it can
   be older than the app. A plan without marks is written without the `marks` field so an older
   helper still crops and rotates; a helper that refuses a request this build wrote reports "The
   picture editing helper is out of date. Rebuild with cargo build" instead of a generic failure.

   **Gallery.** The picture gallery (the media viewer) has an Edit button on pictures (not videos).
   It is never greyed out: pressing it checks the downloaded file's header, not its name or the type
   a sender claimed, and either opens the editor (JPEG, PNG and WebP) or says why not (still
   downloading; or "Only JPEG, PNG and WebP pictures can be edited, and this is something else").
   Done does not change the picture in the chat: the edit is a new picture, added to the message
   being written (so a caption can go with it) and sent with Send. It is dropped, and its file
   deleted, if the conversation moved on in the meantime.

   **Edited copies are temporary.** The result is a file under `~/.cache/monogram-desktop/edited`,
   private and named at random. It is deleted as soon as it is not needed: when the attachment
   is removed or replaced, when the picture has uploaded, when a message still waiting to upload
   is deleted, or when a copy cannot be added. As a safety net for one forgotten (a crash), anything
   older than six hours is swept whenever the editor opens.

   **Pen pressure.** Pressure is read from the same drag gesture that handles the pointer, from
   the event's pressure axis. An earlier version used a separate stylus gesture next to it, which
   the drag gesture's claim on the pen's touch denied, so no pressure ever arrived. To see what a
   device reports, run with `MONOGRAM_DEBUG_PEN=1`: every press and move prints the device name,
   its source, its tool type and the pressure to stderr. A pen that reports no pressure draws one
   width, like a mouse.

   **Layout (built to fit a small window).** The picture takes all the room. Down its left edge is
   a strip of drawing tools (freehand, text, circle, square, arrow); with none chosen, you are cropping
   and drag the frame's corner brackets and edge grips. The header holds Cancel, the mark colour (a
   `GtkColorDialogButton`, so the GNOME colour chooser), the line size (a button showing "12 px"
   that opens a slider, 1 to 128 px, starting at a size that suits the picture), Undo, a **Crop**
   menu and Done. The Crop menu holds the crop shape (Free, Original, Square, 4:3, 16:9), rotate
   left and right, Reset, "Precise Crop" numeric rows and "Mark the Crop Area". The dialog needs
   under 460 px of width and 420 px of height, tested. The crop shapes are icons with the full name
   as tooltip and accessible label, never cut-off text. A 3x3 grid shows over the crop while the
   pointer is over the picture (and during a resize), only while cropping. Keyboard: arrow keys move the crop, Shift
   with arrows resizes it; the numeric rows and "Mark the Crop Area" give a pointer-free,
   screen-reader-friendly path. Done writes a new file under the cache directory
   (`monogram-desktop/edited`, private, pruned after 24 hours and removed with its attachment) that
   replaces the attachment in the same position; the original is never modified. Cancel, Escape or
   closing stops a running edit.

   **Text.** With the Text tool (T), clicking the picture opens a small entry there; Enter puts the
   words on the picture in the chosen colour. The slider then sets the text size (8 to 512 px of
   line height) and remembers it apart from the line size. With the tool chosen and the picture
   focused, Enter asks for text at the crop's corner, the pointer-free way. The words are drawn
   into a coverage bitmap in the editor (so the sandbox needs no fonts), kept upright, and travel
   as a `Mark::Text` whose deflated, base64 mask the helper composites over the picture with
   source-over blending. The bitmap is turned to the source orientation when it is sent, so the
   text reads upright where it was put however the picture is later rotated (tested for every
   combination of turns, painting the real mask). Limits: 200 characters, 4 million bitmap pixels
   per text and 8 million in all, enforced by the editor, by the plan and again by the helper, which
   also refuses a mask that does not inflate to exactly its size. A request can now be about 1 MiB,
   so it is written to the helper from its own thread. Text that is blank, too long or too large
   for the picture is refused with a message.

   **Keyboard.** Ctrl+Z undo, Ctrl+Shift+Z or Ctrl+Y redo, B draw, T text, O circle, R square, A arrow
   (pressing a tool key again lets go of it), V crop frame, C Crop menu, S line size, K colour,
   [ and ] thinner and thicker, Ctrl+Left and Ctrl+Right rotate, Ctrl+Enter Done, Escape cancels.
   The list lives in `EDITOR_SHORTCUTS`, which both binds the keys and fills the app's Shortcuts
   window, so they cannot disagree. Plain letters never fire while a text field has focus.

   **Marks (done: square, circle, arrow and freehand).** Choosing a tool from the strip and dragging
   on the picture draws that shape in the chosen colour and size: a square or circle in the dragged
   box, an arrow from where the drag began to where it ended, a freehand line along the pointer. A pen's pressure sets the width along a freehand line, from a
   hairline (3 percent of the chosen size, never under one pixel) for a feather touch to the full
   chosen size, which an ordinary firm press already reaches (`PRESSURE_GAIN` 1.5, `PRESSURE_CURVE`
   2.2, `PRESSURE_FLOOR` 0.03 in `crop_area.rs`, one place to tune the feel). The reading is eased
   from point to point so a jittery sensor does not ripple the line, and the first and last four
   points of a long line are thinned (`pen_taper`) so it starts and ends on a finer point. The
   width is stored per point in `Mark::Freehand::pressure`; a mouse draws one width.
   "Mark the Crop Area" (in the Crop menu) places the chosen shape without a pointer (a square or
   circle around the crop, an arrow across it from corner to corner, a line along its edge) and
   Undo in the header takes back the newest; Redo appears next to it only after an undo and goes
   when nothing is left to redo or something new is drawn. Marks are stored in view pixels, so they turn with the picture, and
   `Editor::job` converts them to source pixels. They are part of the `Job` and `Plan`
   (`Mark::{Square, Circle, Arrow, Freehand, Text}`; at most 32 marks, 256 points per freehand line and
   1024 points in all, stroke 1 to 128 px, every point inside the picture, images only) and the
   confined helper draws them, never the GTK process: it converts to RGBA, paints through
   `media_authoring::paint` in a pad probe, then crops, rotates and scales, so a crop that leaves
   a mark out drops it. Long lines are thinned to the point cap, keeping both ends. A mark that
   cannot be drawn fails the edit instead of saving an unmarked picture. Tested without a display
   (geometry, limits, JSON, pixel painting of every shape), in the real sandbox (pixels for every
   shape, and with crop plus rotation) and in the dialog (drags, undo, saved file).
4. Video trim and compression with progress and cancel off the main thread.
5. Albums. **Composer done:** several files can be chosen at once, each card shows its number in
   the send order with "Send earlier" and "Send later" buttons (hidden for a single file, disabled
   at either end, so the order can be changed from the keyboard), and one caption covers the set.
   `Ui::move_attachment` reorders `attachments` and the cards together; edited copies keep their
   place. **Not done:** a grouped display and a grouping on the wire. Today each file goes out as its
   own message with the caption on the first, and the timeline only stacks consecutive bubbles from
   one sender. No XEP defines an album. The standard way to put several files in one message is
   XEP-0447 section 4.2 (several `<file-sharing/>` elements with different `id`s, each source in a
   follow-up message that attaches to the first, with a fallback body), which needs a message to
   hold a list of attachments. Doing that, and a grid to show it, is the remaining work, with the
   same behaviour needed in mobile.
6. Camera capture through the desktop camera portal.
