# Camera

Take Photo… in the attach menu opens the camera, and the picture you take is added to your
message. Cameras are reached the way the desktop intends: through the **camera portal**, and
only after the person has agreed.

## How permission works

1. **Take Photo…** is the only way in. Nothing opens a camera by itself, and no `/dev/video*`
   device is ever opened by this app.
2. The app asks `org.freedesktop.portal.Camera` whether a camera exists. With none, it says so
   and asks nothing else. On a desktop without the portal it says that instead.
3. It calls `AccessCamera`. The desktop shows its own permission question; the app only waits.
   The answer is believed only if it comes from the portal itself: a signal from any other
   program on the bus is ignored.
4. Only after a yes does it call `OpenPipeWireRemote`. That returns a PipeWire connection that
   shows camera nodes and nothing else. A no, a closed question or a time-out never leads to a
   connection.
5. GStreamer's `pipewiresrc` speaks the PipeWire protocol on that connection and
   `videoconvert ! videoscale ! videorate` produce RGBA pictures of at most 1920x1080 and 30
   frames a second, whatever the camera offers.
6. Closing the dialog stops the stream and closes the connection, which ends the system's camera
   indicator.

Each outcome has its own page: waiting, live, denied, not given (question closed), no camera,
not available (no portal), no answer, and a general problem page that shows the cause. Try Again
asks again, except where asking again cannot help.

## What is kept

A photo is encoded off the main loop as a JPEG (quality 90, alpha dropped) and saved with mode
0600 in the private temporary-copy folder, the one edited pictures use: it is deleted when the
photo is removed from the message or sent, refused files are deleted at once, and anything older
than six hours is pruned. Nothing is uploaded until you send the message.

## Limits and failure handling

* Frames are checked before use: size at most 1920x1080 and exactly width x height x 4 bytes.
* Pictures cross from the camera's thread through a one-slot channel, so a slow window drops
  pictures instead of queueing them.
* The PipeWire descriptor is held only while streaming and is closed on stop.
* If the GStreamer PipeWire plugin is missing the dialog says so.

## Where it lives

| File | Job |
| --- | --- |
| `src/camera/portal.rs` | the permission steps over GIO's D-Bus (no Tokio runtime needed) |
| `src/camera/stream.rs` | the PipeWire pipeline and its bounded output |
| `src/camera/frame.rs` | one checked picture, for the screen or as a JPEG |
| `src/camera/fake.rs` | a fake portal for tests (test builds only) |
| `src/camera_ui.rs` | the dialog and the attach-menu action |

## Testing

The portal tests run against a fake `org.freedesktop.portal.Desktop` on a **private** session
bus (`tools/headless-gtk-test.sh`), and refuse to run if the bus looks like the real one, where
they could ask for the camera. Streams are tested with GStreamer's `videotestsrc`; no test opens
a real camera. The one thing no test here covers is a real camera behind a real portal.

## Why desktop has no Telegram-style camera button

On mobile (a Telegram-based app) the camera is a dedicated button that expands into the camera,
and holding it records a round video. That is the main way people record there, so it is
implemented in the mobile app (see the mobile docs, `CAMERA_FAB_TRANSITION.md`), and it is not
copied to desktop for now: a hover or hold gesture is a touch idea, desktop cameras are used
rarely, and a desktop person is better served by a plain, permissioned Take Photo… in the attach
menu. Desktop therefore has portal photo capture only. Round-video capture and a dedicated camera
button are not implemented on desktop; they stay on the mobile side until there is a reason to
bring them over.

## Not included

Choosing between several cameras, mirroring the preview, video and video calls, and reading QR
codes live (Device Sync still reads them from an image).
