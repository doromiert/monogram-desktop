# Voice messages

A microphone button sits right after the emoji button in the message box, in the same flat
style, with the GNOME `audio-input-microphone-symbolic` icon.

## Using it

* **Hold** the button to record, **let go** to send. While you hold, the rest of the message box
  becomes a recording bar: a waveform in the accent colour that moves with your voice, and the time
  recorded as `00:00`. The microphone button stays where it is, filled with the accent colour.
* **Slide left** (right in right-to-left languages) far enough from the button and let go to
  **cancel**; the hint turns red and says "Release to cancel". Slide back and it sends again.
* **Esc** or the bin button cancels at any time.
* **Keyboard**: with the button focused, **Enter or Space** starts, and again sends. This works
  for anyone who cannot hold a button down.
* A press shorter than half a second is an accident: nothing is recorded and a note says how it
  works. A recording shorter than one second is not worth sending and is deleted with a note.

Your draft text and any attachments are untouched; the voice message is sent on its own.

## What ends a recording any other way is kept, not sent

A message that you did not clearly choose to send is put in the message box as an attachment to
look at first:

* the 10 minute limit was reached,
* the microphone stopped working part way (unplugged, taken by another program),
* the system took the pointer away mid-press.

If you change conversation (or the account, or close the window) the recording is discarded and
never goes to a different conversation. If it cannot be queued (a blocked contact), it is kept as
an attachment rather than lost.

## Permission

The desktop has no microphone portal (the camera portal does not cover it), so there is no
permission question to show. What is respected:

* the desktop's own **microphone switch** (`org.gnome.desktop.privacy disable-microphone`): when
  it is on, nothing records and a note says so;
* the sound server's refusal (a confined app without access) is shown as "Microphone access was
  refused" with where to allow it;
* nothing is ever recorded without a button held or Enter pressed, and the system's own recording
  indicator shows while the microphone is open.

Other failures say what to do: no microphone found, the microphone is in use by another program.

## How it records

`pipewiresrc` (or `pulsesrc`, then `autoaudiosrc`) → mono 48 kHz → `level` → `opusenc`
(24 kbit/s voice) → `oggmux` → a private (0600) file in the temporary-copy folder. That is Ogg Opus,
which the app's own player and validator accept and other apps play. The file is named
`voice-YYYYMMDD-HHMMSS-xxxxxx.ogg`, and is deleted after upload, on removal, or after six hours.

## Why it does not lag

* The microphone is opened on a worker thread. Starting GStreamer for the first time and
  connecting to the sound server can take over a second; the window does not wait for it.
* Recording and encoding run on GStreamer's own threads.
* The window receives about 20 small level messages a second, which also give the length.
* The waveform keeps a fixed number of bars and draws at most a few dozen rounded rectangles; the
  timer text changes once a second.
* Tests measure the longest pause of the main loop while recording and require it to stay small.

## Where it lives

| File | Job |
| --- | --- |
| `src/voice/recorder.rs` | the pipeline, fallback between sound servers, finishing, limits |
| `src/voice/meter.rs` | loudness to bar height, timer text, the bar history |
| `src/voice/wave.rs` | the waveform widget |
| `src/voice_ui.rs` | the button, the gesture, the recording bar and what happens to a recording |

## Not included

Locking a recording to keep going hands-free, pausing, previewing before sending, and a waveform
stored with the message (the player draws its own from the file).

## Voice intent on the wire

Both clients use the local, unsubmitted [voice-message ProtoXEP](../../mobile/xeps/xep-voice-messages.xml): an empty `voice-message` in `urn:monogram:voice-message:0` belongs inside the particular SFS file metadata. This is not an XSF-assigned namespace. The recorder sets an explicit durable outbox flag, including recordings kept for review; importing an audio file does not. Neither filenames nor codecs imply a voice note.

Mobile projects this into its native voice controls. Desktop keeps its sandboxed waveform player, voice-message previews and accessible control labels. Modern encrypted content protects the marker with the file description. Legacy URI-only OMEMO media deliberately falls back to ordinary audio remotely, without publishing the hint outside encryption or changing the selected encryption mode. Old messages without a marker remain ordinary audio.

Shared Java/Rust vectors cover valid, invalid and forged metadata. Independent-client interoperability and real-microphone acceptance remain unverified.
