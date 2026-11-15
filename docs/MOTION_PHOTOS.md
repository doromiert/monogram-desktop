# Motion photos

Mobile and desktop share the original JPEG Motion Photo container through the existing
[XEP-0447](https://xmpp.org/extensions/xep-0447.html) attachment path, with
[XEP-0446](https://xmpp.org/extensions/xep-0446.html) image/jpeg metadata. The still
JPEG contains Google Camera/Container XMP and an appended MP4, following the
[Android Motion Photo format](https://developer.android.com/media/platform/motion-photo-format).
No private stanza extension, second message or unprotected video URL is used.
Clients without motion-photo support can show the ordinary JPEG still.

Mobile preserves the original container when no video edit is necessary. Edited
clips are rebuilt with the full still using its existing MediaController writer.
Its native live-photo UI receives the full photo plus a local extracted document;
sharing/forwarding/retrying retain the original complete attachment. Desktop sends
selected motion JPEGs unchanged, just like other attachments.

Desktop shows the original still. Clicking the image or its accessible Play button
plays once in place; Stop, end of playback, leaving the viewport, closing the viewer,
or changing account restores the still and releases the player. The expanded media
viewer also has Play/Stop and image-click playback. Keyboard users can activate the
Play/Stop buttons with Enter or Space. Motion photos do not autoplay. Other media
keeps its existing playback behavior.

Both the still and video are covered by the same file digest and existing encrypted
file transfer. Desktop parses only bounded JPEG metadata (2 MiB/256 segments), with
namespace-aware XMP; it validates a positive embedded MP4 length, a contained offset
and an ftyp box. Google MicroVideo v1 files remain readable. Invalid/stale XMP falls
back to the JPEG still. Video extraction is capped at 512 MiB, runs after whole-file
verification on a worker, and writes to a private unlinked disk-backed descriptor.
It never leaves a separate persistent decrypted clip. Image decoding and media
plugin initialization remain off GTK's thread; a preparing spinner appears immediately.
Cached previews retain motion metadata without changing immutable received attachments.

This implementation reads JPEG motion-photo containers, matching mobile's picker.
HEIC/AVIF motion-container parsing and separate Apple HEIC+MOV library pairs are not
implemented. Existing MP4-only messages cannot recover a still photo that was never sent.

## Verification

- `motion_photo::tests`: Android's actual MediaController writer fixture, plain and
  encrypted SFS metadata, JSON persistence, exact clip extraction, malformed/stale
  XML, length overflow and invalid ftyp rejection.
- `motion_photo_ui::tests::chat_motion_photo_starts_still_plays_once_returns_still_and_replays`
  (isolated native display/Secret Service): real decoding and Clapper playback,
  still-first behavior, immediate feedback, natural end, replay/stop, unmap,
  expanded viewer, unchanged original attachment and persisted preview metadata.
- `tests/fixtures/motion-photoMP.jpg` is a synthetic solid-color 3200×2400 image
  produced by mobile's real writer with the existing reaction-video.mp4 fixture.
